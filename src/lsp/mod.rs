pub mod transport;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::{Value, json};

use crate::cache::{self, CacheFile, Stamp};
use crate::features;
use crate::index::ScratchIndex;
use crate::lanes::{LaneSizes, Lanes};
use crate::workspace::{FileKind, HeaderSet, Layout, Settings, Slot, SlotMap, Snapshot, discover, file_id, path_id, path_to_uri, synthetic_index};

use transport::{Connection, read_message, write_message};

pub type SessionId = u32;

pub struct Config {
    pub project: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    pub no_cache: bool,
    pub lanes: LaneSizes,
    pub daemon: bool,
    pub idle_exit: Option<std::time::Duration>,
}

pub enum Event {
    Connected(SessionId, Sender<Vec<u8>>),
    Message(SessionId, Vec<u8>),
    Closed(SessionId),
    Loaded(Box<Loaded>),
    RefreshDone(u64),
    IdleCheck(u64),
}

pub struct Loaded {
    pub layout: Layout,
    pub texts: Vec<(PathBuf, FileKind, Arc<str>)>,
    pub index: Option<Arc<ScratchIndex>>,
    pub index_stamp: Option<Stamp>,
    pub index_error: Option<String>,
    pub cached: FxHashMap<u64, Arc<crate::workspace::Derived>>,
}

pub fn load(root: &std::path::Path, cache_path: Option<&std::path::Path>) -> Loaded {
    let layout = discover(root);
    let texts: Vec<(PathBuf, FileKind, Arc<str>)> = layout
        .files
        .iter()
        .filter_map(|(path, kind)| std::fs::read(path).ok().map(|b| (path.clone(), kind.clone(), Arc::from(String::from_utf8_lossy(&b).as_ref()))))
        .collect();
    let cached_file = cache_path.and_then(cache::load);
    let (mut index, mut index_stamp, mut index_error) = (None, None, None);
    if let Some(source) = &layout.index_source {
        let stamp = Stamp::of(source);
        let from_cache = cached_file.as_ref().and_then(|c| c.index.as_ref()).filter(|(s, _)| Some(s) == stamp.as_ref()).map(|(_, data)| data.clone());
        match from_cache.map(Ok).unwrap_or_else(|| crate::loader::load_index(source)) {
            Ok(data) => {
                index = Some(ScratchIndex::new(data, true));
                index_stamp = stamp;
            }
            Err(e) => index_error = Some(e),
        }
    }
    let cached = cached_file.map(|c| c.entries.into_iter().collect()).unwrap_or_default();
    Loaded { layout, texts, index, index_stamp, index_error, cached }
}

#[derive(Default)]
struct ClientCaps {
    work_done_progress: bool,
    watched_files_dynamic: bool,
}

struct Session {
    out: Sender<Vec<u8>>,
    open: FxHashSet<Arc<str>>,
    caps: ClientCaps,
    initialized: bool,
    shutdown: bool,
}

type Published = FxHashMap<Arc<str>, (u64, u64)>;

#[derive(Clone)]
pub struct Outbox {
    sessions: Arc<Mutex<FxHashMap<SessionId, Sender<Vec<u8>>>>>,
    published: Arc<Mutex<Published>>,
    request_ids: Arc<AtomicU64>,
}

impl Outbox {
    fn new() -> Outbox {
        Outbox { sessions: Arc::default(), published: Arc::default(), request_ids: Arc::new(AtomicU64::new(1)) }
    }

    pub fn broadcast(&self, message: &Value) {
        let bytes = serde_json::to_vec(message).unwrap_or_default();
        for out in self.sessions.lock().unwrap_or_else(|e| e.into_inner()).values() {
            let _ = out.send(bytes.clone());
        }
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.broadcast(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    pub fn request(&self, method: &str, params: Value) {
        let id = format!("scratchpilerd/{}", self.request_ids.fetch_add(1, Ordering::Relaxed));
        self.broadcast(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    pub fn publish(&self, uri: &Arc<str>, generation: u64, version: Option<i32>, diagnostics: Value) {
        let hash = xxhash_rust::xxh3::xxh3_64(diagnostics.to_string().as_bytes());
        {
            let mut published = self.published.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((last_generation, last_hash)) = published.get(uri)
                && (*last_generation > generation || *last_hash == hash)
            {
                return;
            }
            published.insert(uri.clone(), (generation, hash));
        }
        let mut params = json!({ "uri": &**uri, "diagnostics": diagnostics });
        if let Some(v) = version {
            params["version"] = json!(v);
        }
        self.notify("textDocument/publishDiagnostics", params);
    }
}

pub struct Ctx {
    pub out: Outbox,
    pub cancelled: Mutex<FxHashSet<String>>,
    pub progress_supported: std::sync::atomic::AtomicBool,
}

struct Core {
    shared: Arc<crate::workspace::Shared>,
    config: Config,
    ctx: Arc<Ctx>,
    events: Sender<Event>,
    lanes: Arc<Lanes>,
    sessions: FxHashMap<SessionId, Session>,
    root: Option<Arc<PathBuf>>,
    layout: Option<Layout>,
    index: Arc<ScratchIndex>,
    index_loaded: bool,
    pushed_index: bool,
    headers: Arc<HeaderSet>,
    settings: Arc<Settings>,
    slots: Arc<SlotMap>,
    disk_backed: FxHashSet<Arc<str>>,
    generation: u64,
    snapshot: Arc<Snapshot>,
    refresh_running: bool,
    refresh_dirty: bool,
    persist_dirty: bool,
    loaded: bool,
    index_stamp: Option<Stamp>,
    idle_token: u64,
}

fn response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn send(out: &Sender<Vec<u8>>, message: &Value) {
    let _ = out.send(serde_json::to_vec(message).unwrap_or_default());
}

fn spawn_writer(mut writer: Box<dyn std::io::Write + Send>, framing: transport::Framing) -> Sender<Vec<u8>> {
    let (tx, rx) = channel::<Vec<u8>>();
    std::thread::Builder::new()
        .name("sp-writer".into())
        .spawn(move || {
            for body in rx {
                if write_message(&mut *writer, framing, &body).is_err() {
                    break;
                }
            }
        })
        .expect("writer thread");
    tx
}

pub fn spawn_session(id: SessionId, connection: Connection, events: Sender<Event>) {
    let Connection { mut reader, writer, framing } = connection;
    let out = spawn_writer(writer, framing);
    let _ = events.send(Event::Connected(id, out));
    std::thread::Builder::new()
        .name(format!("sp-reader-{id}"))
        .spawn(move || {
            while let Ok(Some(body)) = read_message(&mut *reader, framing) {
                if events.send(Event::Message(id, body)).is_err() {
                    return;
                }
            }
            let _ = events.send(Event::Closed(id));
        })
        .expect("reader thread");
}

pub fn run(config: Config, connection: Option<Connection>, listener: Option<transport::Listener>) -> i32 {
    let (events, inbox) = channel::<Event>();
    if let Some(connection) = connection {
        spawn_session(0, connection, events.clone());
    }
    if let Some(listener) = listener {
        let events = events.clone();
        std::thread::Builder::new()
            .name("sp-listener".into())
            .spawn(move || {
                let mut next = 1;
                while let Ok(connection) = listener.accept() {
                    spawn_session(next, connection, events.clone());
                    next += 1;
                }
            })
            .expect("listener thread");
    }
    let lanes = Arc::new(Lanes::new(config.lanes));
    let ctx = Arc::new(Ctx { out: Outbox::new(), cancelled: Mutex::default(), progress_supported: Default::default() });
    let index = ScratchIndex::empty();
    let headers = HeaderSet::new(FxHashMap::default());
    let settings = Arc::new(Settings::default());
    let slots = Arc::new(SlotMap::default());
    let shared = Arc::new(crate::workspace::Shared::default());
    let snapshot = Arc::new(Snapshot::new(shared.clone(), 0, Arc::new(PathBuf::new()), index.clone(), false, headers.clone(), settings.clone(), slots.clone()));
    let mut core = Core {
        shared,
        config,
        ctx,
        events,
        lanes,
        sessions: FxHashMap::default(),
        root: None,
        layout: None,
        index,
        index_loaded: false,
        pushed_index: false,
        headers,
        settings,
        slots,
        disk_backed: FxHashSet::default(),
        generation: 0,
        snapshot,
        refresh_running: false,
        refresh_dirty: false,
        persist_dirty: false,
        loaded: false,
        index_stamp: None,
        idle_token: 0,
    };
    if core.config.daemon && core.config.project.is_some() {
        core.root = core.config.project.clone().map(|p| Arc::new(crate::workspace::canonical(&p).unwrap_or(p)));
        core.start_load();
        core.arm_idle_exit();
    }
    core.main_loop(inbox)
}

impl Core {
    fn main_loop(&mut self, inbox: Receiver<Event>) -> i32 {
        while let Ok(event) = inbox.recv() {
            match event {
                Event::IdleCheck(token) => {
                    if token == self.idle_token && self.sessions.is_empty() {
                        self.persist_now();
                        return 0;
                    }
                }
                Event::Connected(id, out) => {
                    self.idle_token += 1;
                    self.ctx.out.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(id, out.clone());
                    self.sessions.insert(id, Session { out, open: FxHashSet::default(), caps: ClientCaps::default(), initialized: false, shutdown: false });
                }
                Event::Message(id, body) => {
                    if let Some(code) = self.on_message(id, &body) {
                        self.persist_now();
                        return code;
                    }
                }
                Event::Closed(id) => {
                    self.ctx.out.sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                    let session = self.sessions.remove(&id);
                    if !self.config.daemon {
                        self.persist_now();
                        return if session.is_some_and(|s| s.shutdown) { 0 } else { 1 };
                    }
                    if let Some(session) = session {
                        for doc in session.open {
                            if !self.sessions.values().any(|s| s.open.contains(&doc)) {
                                self.close_document(&doc);
                            }
                        }
                    }
                    if self.sessions.is_empty() {
                        self.arm_idle_exit();
                    }
                }
                Event::Loaded(loaded) => self.on_loaded(*loaded),
                Event::RefreshDone(generation) => {
                    self.refresh_running = false;
                    if self.refresh_dirty || generation < self.generation {
                        self.refresh_dirty = false;
                        self.schedule_refresh();
                    } else if self.persist_dirty {
                        self.persist_dirty = false;
                        self.persist();
                    }
                }
            }
        }
        0
    }

    fn on_message(&mut self, session: SessionId, body: &[u8]) -> Option<i32> {
        let message: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => {
                if let Some(s) = self.sessions.get(&session) {
                    send(&s.out, &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("Parse error: {e}") } }));
                }
                return None;
            }
        };
        let method = message.get("method").and_then(Value::as_str).map(str::to_string)?;
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match (method.as_str(), id) {
            ("initialize", Some(id)) => self.initialize(session, &id, &params),
            ("initialized", None) => self.initialized(session),
            ("shutdown", Some(id)) => {
                if let Some(s) = self.sessions.get_mut(&session) {
                    s.shutdown = true;
                    send(&s.out, &response(&id, Value::Null));
                }
            }
            ("exit", None) => {
                if !self.config.daemon {
                    let clean = self.sessions.get(&session).is_some_and(|s| s.shutdown);
                    return Some(if clean { 0 } else { 1 });
                }
            }
            ("$/cancelRequest", None) => {
                if let Some(id) = params.get("id") {
                    self.ctx.cancelled.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string());
                }
            }
            ("textDocument/didOpen", None) => self.did_open(session, &params),
            ("textDocument/didChange", None) => self.did_change(&params),
            ("textDocument/didClose", None) => self.did_close(session, &params),
            ("textDocument/didSave", None) => self.did_save(&params),
            ("workspace/didChangeWatchedFiles", None) => self.watched_files_changed(&params),
            ("workspace/didChangeConfiguration", None) => self.configuration_changed(&params),
            ("scratchpiler/setIndex", None) => self.set_index(&params),
            (_, Some(id)) => self.request(session, method, id, params),
            _ => {}
        }
        None
    }

    fn initialize(&mut self, session: SessionId, id: &Value, params: &Value) {
        let options = params.get("initializationOptions").cloned().unwrap_or(Value::Null);
        let requested = options.get("project").and_then(Value::as_str).map(PathBuf::from).or_else(|| self.config.project.clone());
        let Some(out) = self.sessions.get(&session).map(|s| s.out.clone()) else { return };
        let Some(project) = requested else {
            send(
                &out,
                &error_response(id, -32602, "scratchpilerd needs the project folder: start it with --project <dir> or pass initializationOptions.project"),
            );
            return;
        };
        let project = crate::workspace::canonical(&project).unwrap_or(project);
        if !project.is_dir() {
            send(&out, &error_response(id, -32602, &format!("scratchpilerd: project folder {} does not exist", project.display())));
            return;
        }
        if let Some(root) = &self.root
            && **root != project
        {
            send(&out, &error_response(id, -32602, &format!("this scratchpilerd serves {}, not {}", root.display(), project.display())));
            return;
        }
        if let Some(settings) = options.get("settings") {
            self.apply_settings(settings);
        }
        let caps = &params["capabilities"];
        if let Some(s) = self.sessions.get_mut(&session) {
            s.caps.work_done_progress = caps["window"]["workDoneProgress"].as_bool().unwrap_or(false);
            s.caps.watched_files_dynamic = caps["workspace"]["didChangeWatchedFiles"]["dynamicRegistration"].as_bool().unwrap_or(false);
            if s.caps.work_done_progress {
                self.ctx.progress_supported.store(true, Ordering::Relaxed);
            }
        }
        send(&out, &response(id, features::capabilities()));
        if self.root.is_none() {
            self.root = Some(Arc::new(project));
            self.start_load();
        }
    }

    fn initialized(&mut self, session: SessionId) {
        let Some(s) = self.sessions.get_mut(&session) else { return };
        s.initialized = true;
        if s.caps.watched_files_dynamic {
            let watchers: Vec<Value> =
                ["**/*.sdsl", "**/*.h", "**/project.json", "**/*.sb3", "**/scratchpiler.json"].iter().map(|g| json!({ "globPattern": g })).collect();
            let id = format!("scratchpilerd/{}", self.ctx.out.request_ids.fetch_add(1, Ordering::Relaxed));
            send(
                &s.out,
                &json!({ "jsonrpc": "2.0", "id": id, "method": "client/registerCapability", "params": { "registrations": [{
                    "id": "scratchpilerd-watch", "method": "workspace/didChangeWatchedFiles", "registerOptions": { "watchers": watchers } }] } }),
            );
        }
        if self.loaded {
            self.republish_all();
        }
    }

    fn apply_settings(&mut self, value: &Value) {
        let value = value.get("scratchpiler").unwrap_or(value);
        let mut merged = serde_json::to_value(&*self.settings).unwrap_or(Value::Null);
        if let (Some(target), Some(source)) = (merged.as_object_mut(), value.as_object()) {
            for (k, v) in source {
                target.insert(k.clone(), v.clone());
            }
        }
        if let Ok(settings) = serde_json::from_value::<Settings>(merged)
            && settings != *self.settings
        {
            self.settings = Arc::new(settings);
            self.bump();
            self.republish_all();
        }
    }

    fn arm_idle_exit(&mut self) {
        let Some(after) = self.config.idle_exit else { return };
        self.idle_token += 1;
        let (token, events) = (self.idle_token, self.events.clone());
        std::thread::spawn(move || {
            std::thread::sleep(after);
            let _ = events.send(Event::IdleCheck(token));
        });
    }

    fn persist_now(&mut self) {
        let Some(path) = self.cache_path() else { return };
        if let Some(file) = self.cache_file() {
            let _ = cache::store(&path, &file);
        }
    }

    fn cache_file(&self) -> Option<CacheFile> {
        if !self.loaded {
            return None;
        }
        let index = self.layout.as_ref().and_then(|l| l.index_source.as_ref()).and(self.index_stamp.clone()).map(|stamp| (stamp, self.index.data.clone()));
        let entries = self.slots.values().filter(|s| self.disk_backed.contains(&s.id)).filter_map(|s| s.derived.get().map(|d| (s.key, d.clone()))).collect();
        Some(CacheFile { format: crate::CACHE_FORMAT.to_string(), index, entries })
    }

    fn cache_path(&self) -> Option<PathBuf> {
        if !self.settings.cache || self.config.no_cache {
            return None;
        }
        let root = self.root.as_ref()?;
        let dir = self.config.cache_dir.clone().or_else(cache::default_dir)?;
        Some(cache::file_for(&dir, root))
    }

    fn start_load(&mut self) {
        let Some(root) = self.root.clone() else { return };
        let events = self.events.clone();
        let cache_path = self.cache_path();
        self.lanes.background(move || {
            let _ = events.send(Event::Loaded(Box::new(load(&root, cache_path.as_deref()))));
        });
    }

    fn on_loaded(&mut self, loaded: Loaded) {
        let Loaded { layout, texts, index, index_stamp, index_error, cached } = loaded;
        for problem in layout.problems.iter().chain(index_error.iter()) {
            self.ctx.out.notify("window/showMessage", json!({ "type": 2, "message": format!("scratchpilerd: {problem}") }));
        }
        if !self.pushed_index {
            self.index = match index {
                Some(index) => index,
                None => ScratchIndex::new(synthetic_index(layout.files.iter().filter_map(|(_, k)| k.sprite().map(str::to_string))), false),
            };
        }
        self.index_stamp = index_stamp;
        self.index_loaded = true;
        let mut headers = FxHashMap::default();
        for (_, kind, text) in &texts {
            if let FileKind::Header(name) = kind {
                headers.insert(name.to_string(), text.clone());
            }
        }
        for slot in self.slots.values().filter(|s| s.open) {
            if let FileKind::Header(name) = &slot.kind {
                headers.insert(name.to_string(), slot.text.clone());
            }
        }
        self.headers = HeaderSet::new(headers);
        let mut slots = SlotMap::default();
        for (path, kind, text) in texts {
            let id = path_id(&path);
            if let Some(open) = self.slots.get(&id) {
                slots.insert(
                    id,
                    Arc::new(Slot::new(open.id.clone(), open.uri.clone(), kind, open.text.clone(), open.version, true, &self.index, &self.headers)),
                );
                continue;
            }
            self.disk_backed.insert(id.clone());
            slots.insert(id.clone(), Arc::new(Slot::new(id, path_to_uri(&path).into(), kind, text, None, false, &self.index, &self.headers)));
        }
        for (id, slot) in self.slots.iter() {
            if !slots.contains_key(id) {
                slots.insert(
                    id.clone(),
                    Arc::new(Slot::new(
                        slot.id.clone(),
                        slot.uri.clone(),
                        slot.kind.clone(),
                        slot.text.clone(),
                        slot.version,
                        slot.open,
                        &self.index,
                        &self.headers,
                    )),
                );
            }
        }
        let mut hits = 0;
        for slot in slots.values() {
            if let Some(derived) = cached.get(&slot.key) {
                let _ = slot.derived.set(derived.clone());
                hits += 1;
            }
        }
        self.persist_dirty = hits != slots.len() || self.index_stamp.is_some();
        self.layout = Some(layout);
        self.slots = Arc::new(slots);
        self.loaded = true;
        self.bump();
        self.schedule_refresh();
    }

    fn bump(&mut self) {
        self.generation += 1;
        let root = self.root.clone().unwrap_or_default();
        self.snapshot = Arc::new(Snapshot::new(
            self.shared.clone(),
            self.generation,
            root,
            self.index.clone(),
            self.index_loaded,
            self.headers.clone(),
            self.settings.clone(),
            self.slots.clone(),
        ));
    }

    fn schedule_refresh(&mut self) {
        if !self.loaded {
            return;
        }
        if self.refresh_running {
            self.refresh_dirty = true;
            return;
        }
        self.refresh_running = true;
        let snapshot = self.snapshot.clone();
        let ctx = self.ctx.clone();
        let events = self.events.clone();
        let lanes = self.lanes.clone();
        self.lanes.background(move || {
            features::diagnostics::refresh(&snapshot, &ctx, &lanes);
            let _ = events.send(Event::RefreshDone(snapshot.generation));
        });
    }

    fn submit_edit(&mut self, id: &Arc<str>) {
        let snapshot = self.snapshot.clone();
        let ctx = self.ctx.clone();
        let target = id.clone();
        self.lanes.edits.submit(id.to_string(), Box::new(move || features::diagnostics::publish_one(&snapshot, &ctx, &target)));
    }

    fn replace_slot(&mut self, slot: Slot) -> Arc<str> {
        let id = slot.id.clone();
        let header_changed = match &slot.kind {
            FileKind::Header(name) => self.headers.headers.get(&**name).map(|t| **t != *slot.text).unwrap_or(true),
            _ => false,
        };
        let header_name = match &slot.kind {
            FileKind::Header(name) => Some(name.to_string()),
            _ => None,
        };
        let text = slot.text.clone();
        Arc::make_mut(&mut self.slots).insert(id.clone(), Arc::new(slot));
        if header_changed {
            let mut headers = self.headers.headers.clone();
            headers.insert(header_name.unwrap_or_default(), text);
            self.headers = HeaderSet::new(headers);
            self.rebind_all();
        }
        self.bump();
        id
    }

    fn rebind_all(&mut self) {
        let slots: SlotMap = self
            .slots
            .iter()
            .map(|(id, s)| {
                let mut fresh = Slot::new(s.id.clone(), s.uri.clone(), s.kind.clone(), s.text.clone(), s.version, s.open, &self.index, &self.headers);
                if fresh.key == s.key {
                    fresh.analysis = s.analysis.clone();
                    fresh.derived = s.derived.clone();
                }
                (id.clone(), Arc::new(fresh))
            })
            .collect();
        self.slots = Arc::new(slots);
    }

    fn id_for(&self, uri: &str) -> Arc<str> {
        let id = file_id(uri);
        if self.slots.contains_key(&id) {
            return id;
        }
        if let Some(slot) = self.slots.values().find(|s| &*s.uri == uri) {
            return slot.id.clone();
        }
        crate::workspace::canonical_file_id(uri)
    }

    fn kind_for(&self, uri: &str) -> FileKind {
        let path = crate::workspace::uri_to_path(uri).unwrap_or_else(|| PathBuf::from(uri));
        let id = self.id_for(uri);
        if let Some(slot) = self.slots.get(&id) {
            return slot.kind.clone();
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.ends_with(".h") {
            return FileKind::Header(name.into());
        }
        let stem = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        FileKind::Sprite(crate::workspace::decode_sprite_file(&stem).into())
    }

    fn did_open(&mut self, session: SessionId, params: &Value) {
        let doc = &params["textDocument"];
        let Some(uri) = doc["uri"].as_str() else { return };
        let text: Arc<str> = doc["text"].as_str().unwrap_or_default().into();
        let version = doc["version"].as_i64().map(|v| v as i32);
        let id = self.id_for(uri);
        let slot = match self.slots.get(&id) {
            Some(existing) => existing.with(uri.into(), text, version, true),
            None => Slot::new(id.clone(), uri.into(), self.kind_for(uri), text, version, true, &self.index, &self.headers),
        };
        if let Some(s) = self.sessions.get_mut(&session) {
            s.open.insert(id.clone());
        }
        let id = self.replace_slot(slot);
        self.submit_edit(&id);
        self.schedule_refresh();
    }

    fn did_change(&mut self, params: &Value) {
        let Some(uri) = params["textDocument"]["uri"].as_str() else { return };
        let id = self.id_for(uri);
        let Some(existing) = self.slots.get(&id).cloned() else { return };
        let changes = params["contentChanges"].as_array().cloned().unwrap_or_default();
        let edits: Vec<crate::text::Edit<'_>> = changes
            .iter()
            .map(|c| crate::text::Edit {
                range: c.get("range").map(|r| {
                    let n = |p: &str, k: &str| r[p][k].as_u64().unwrap_or(0) as u32;
                    (n("start", "line"), n("start", "character"), n("end", "line"), n("end", "character"))
                }),
                text: c["text"].as_str().unwrap_or_default(),
            })
            .collect();
        let text: Arc<str> = crate::text::apply_edits(&existing.text, &edits).into();
        let version = params["textDocument"]["version"].as_i64().map(|v| v as i32);
        let slot = existing.with(existing.uri.clone(), text, version, true);
        self.disk_backed.remove(&id);
        let id = self.replace_slot(slot);
        self.submit_edit(&id);
        self.schedule_refresh();
    }

    fn close_document(&mut self, id: &Arc<str>) {
        let Some(existing) = self.slots.get(id).cloned() else { return };
        let on_disk = existing.uri.starts_with("file:") && crate::workspace::uri_to_path(&existing.uri).is_some_and(|p| p.is_file());
        let in_layout = self.layout.as_ref().is_some_and(|l| l.files.iter().any(|(p, _)| path_id(p) == *id));
        if !in_layout || !on_disk {
            Arc::make_mut(&mut self.slots).shift_remove(id);
            self.ctx.out.publish(&existing.uri, self.generation + 1, None, json!([]));
            self.bump();
            self.schedule_refresh();
            return;
        }
        let path = crate::workspace::uri_to_path(&existing.uri).unwrap_or_default();
        let text: Arc<str> = std::fs::read_to_string(&path).map(Arc::from).unwrap_or_else(|_| existing.text.clone());
        let mut slot = existing.with(existing.uri.clone(), text, None, false);
        slot.analysis = Arc::default();
        if !slot.has_derived() {
            self.disk_backed.insert(id.clone());
        }
        self.replace_slot(slot);
        self.schedule_refresh();
    }

    fn did_close(&mut self, session: SessionId, params: &Value) {
        let Some(uri) = params["textDocument"]["uri"].as_str() else { return };
        let id = self.id_for(uri);
        if let Some(s) = self.sessions.get_mut(&session) {
            s.open.remove(&id);
        }
        if !self.sessions.values().any(|s| s.open.contains(&id)) {
            self.close_document(&id);
        }
    }

    fn did_save(&mut self, params: &Value) {
        let Some(uri) = params["textDocument"]["uri"].as_str() else { return };
        let id = self.id_for(uri);
        if let Some(text) = params.get("text").and_then(Value::as_str)
            && let Some(existing) = self.slots.get(&id).cloned()
        {
            let slot = existing.with(existing.uri.clone(), text.into(), existing.version, existing.open);
            self.replace_slot(slot);
        }
        self.disk_backed.insert(id);
        self.persist_dirty = true;
        self.schedule_refresh();
    }

    fn watched_files_changed(&mut self, params: &Value) {
        let mut relayout = false;
        for change in params["changes"].as_array().cloned().unwrap_or_default() {
            let Some(uri) = change["uri"].as_str() else { continue };
            let Some(path) = crate::workspace::uri_to_path(uri) else { continue };
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let id = self.id_for(uri);
            let kind = change["type"].as_u64().unwrap_or(2);
            if name == "project.json" || name == "scratchpiler.json" || name.to_lowercase().ends_with(".sb3") || kind != 2 {
                relayout = true;
                continue;
            }
            let Some(existing) = self.slots.get(&id).cloned() else {
                relayout = true;
                continue;
            };
            if existing.open {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path)
                && *text != *existing.text
            {
                let slot = existing.with(existing.uri.clone(), text.into(), None, false);
                self.disk_backed.insert(id);
                self.replace_slot(slot);
                self.persist_dirty = true;
            }
        }
        if relayout {
            self.reload();
        } else {
            self.schedule_refresh();
        }
    }

    fn reload(&mut self) {
        self.loaded = false;
        self.start_load();
    }

    fn configuration_changed(&mut self, params: &Value) {
        if let Some(settings) = params.get("settings") {
            self.apply_settings(settings);
        }
    }

    fn set_index(&mut self, params: &Value) {
        let Ok(data) = serde_json::from_value::<crate::index::IndexData>(params.get("index").cloned().unwrap_or_else(|| params.clone())) else { return };
        self.index = ScratchIndex::new(data, true);
        self.pushed_index = true;
        self.index_loaded = true;
        self.rebind_all();
        self.bump();
        self.schedule_refresh();
    }

    fn republish_all(&mut self) {
        self.ctx.out.published.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.schedule_refresh();
    }

    fn persist(&mut self) {
        let Some(path) = self.cache_path() else { return };
        let Some(file) = self.cache_file() else { return };
        self.lanes.background(move || {
            if let Err(e) = cache::store(&path, &file) {
                eprintln!("scratchpilerd: could not write cache {}: {e}", path.display());
            }
        });
    }

    fn request(&mut self, session: SessionId, method: String, id: Value, params: Value) {
        let Some(out) = self.sessions.get(&session).map(|s| s.out.clone()) else { return };
        let snapshot = self.snapshot.clone();
        let ctx = self.ctx.clone();
        self.lanes.interactive(move || {
            let key = id.to_string();
            let cancelled = |ctx: &Ctx| ctx.cancelled.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
            if cancelled(&ctx) {
                send(&out, &error_response(&id, -32800, "cancelled"));
                return;
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| features::request(&snapshot, &method, &params)));
            let bytes = match result {
                Ok(Ok(features::Body::Json(value))) => serde_json::to_vec(&response(&id, value)).unwrap_or_default(),
                Ok(Ok(features::Body::Raw(raw))) => format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{raw}}}").into_bytes(),
                Ok(Err((code, message))) => serde_json::to_vec(&error_response(&id, code, &message)).unwrap_or_default(),
                Err(_) => serde_json::to_vec(&error_response(&id, -32603, &format!("scratchpilerd: {method} failed"))).unwrap_or_default(),
            };
            if cancelled(&ctx) {
                send(&out, &error_response(&id, -32800, "cancelled"));
            } else {
                let _ = out.send(bytes);
            }
        });
    }
}
