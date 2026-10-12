use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use indexmap::IndexMap;
use rustc_hash::{FxBuildHasher, FxHashMap};
use serde::{Deserialize, Serialize};

use crate::analyzer::{Analysis, analyze, local_diagnostics};
use crate::diag::{Diag, LintSettings};
use crate::facts::{FileFacts, extract_file_facts};
use crate::index::{IndexData, STAGE, ScratchIndex, SpriteInfo};
use crate::preprocess::Headers;
use crate::project::{LinkOptions, Linked, link};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    #[serde(flatten)]
    pub lint: LintSettings,
    pub semantic_tokens: SemanticTokenMode,
    pub inlay_hints: bool,
    pub completion_follow_ups: bool,
    pub completion_ranking: bool,
    pub cache: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SemanticTokenMode {
    Full,
    Semantic,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            lint: LintSettings::default(),
            semantic_tokens: SemanticTokenMode::Full,
            inlay_hints: true,
            completion_follow_ups: true,
            completion_ranking: true,
            cache: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileKind {
    Sprite(Arc<str>),
    Header(Arc<str>),
}

impl FileKind {
    pub fn sprite(&self) -> Option<&str> {
        match self {
            FileKind::Sprite(s) => Some(s),
            FileKind::Header(_) => None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Manifest {
    project: Option<String>,
    stage: Option<String>,
    sprites: IndexMap<String, String>,
    headers: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub root: PathBuf,
    pub index_source: Option<PathBuf>,
    pub files: Vec<(PathBuf, FileKind)>,
    pub problems: Vec<String>,
}

fn hex_byte(bytes: &[u8], i: usize) -> Option<u8> {
    let digit = |b: u8| (b as char).to_digit(16);
    if bytes[i] != b'%' || i + 2 >= bytes.len() {
        return None;
    }
    Some((digit(bytes[i + 1])? * 16 + digit(bytes[i + 2])?) as u8)
}

pub fn decode_sprite_file(stem: &str) -> String {
    if stem == "Stage" {
        return STAGE.to_string();
    }
    let bytes = stem.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if let Some(v) = hex_byte(bytes, i) {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn encode_sprite_file(name: &str) -> String {
    if name == STAGE {
        return "Stage.sdsl".into();
    }
    let mut out = String::new();
    for c in name.chars() {
        if matches!(c, '%' | '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 0x20 {
            out.push_str(&format!("%{:02X}", c as u32));
        } else {
            out.push(c);
        }
    }
    out + ".sdsl"
}

fn list_dir(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    entries.sort();
    entries
}

fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

pub fn discover(root: &Path) -> Layout {
    let mut problems = Vec::new();
    let manifest: Manifest = match std::fs::read_to_string(root.join("scratchpiler.json")) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            problems.push(format!("scratchpiler.json: {e}"));
            Manifest::default()
        }),
        Err(_) => Manifest::default(),
    };
    let entries = list_dir(root);
    let index_source = match &manifest.project {
        Some(p) => Some(root.join(p)),
        None => {
            let json = root.join("project.json");
            if json.is_file() {
                Some(json)
            } else {
                let sb3s: Vec<&PathBuf> = entries.iter().filter(|p| has_ext(p, "sb3")).collect();
                match sb3s.as_slice() {
                    [] => None,
                    [one] => Some((*one).clone()),
                    many => {
                        problems.push(format!("{} .sb3 files in the project folder; name one in scratchpiler.json \"project\"", many.len()));
                        None
                    }
                }
            }
        }
    };

    let mut files: Vec<(PathBuf, FileKind)> = Vec::new();
    let mut mapped: Vec<PathBuf> = Vec::new();
    if let Some(stage) = &manifest.stage {
        let path = root.join(stage);
        mapped.push(path.clone());
        files.push((path, FileKind::Sprite(STAGE.into())));
    }
    for (sprite, file) in &manifest.sprites {
        let path = root.join(file);
        mapped.push(path.clone());
        let name = if sprite == "Stage" || sprite == STAGE { STAGE } else { sprite.as_str() };
        files.push((path, FileKind::Sprite(name.into())));
    }
    for path in &entries {
        if !has_ext(path, "sdsl") || mapped.contains(path) {
            continue;
        }
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let sprite = decode_sprite_file(&stem);
        if files.iter().any(|(_, k)| k.sprite() == Some(sprite.as_str())) {
            continue;
        }
        files.push((path.clone(), FileKind::Sprite(sprite.into())));
    }
    let mut header_dirs = vec![root.to_path_buf(), root.join("include"), root.join("headers")];
    header_dirs.extend(manifest.headers.iter().map(|h| root.join(h)));
    for dir in header_dirs {
        for path in list_dir(&dir) {
            if has_ext(&path, "h") && crate::preprocess::is_header_name(&path.file_name().unwrap_or_default().to_string_lossy()) {
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                if !files.iter().any(|(_, k)| matches!(k, FileKind::Header(n) if **n == *name)) {
                    files.push((path, FileKind::Header(name.into())));
                }
            }
        }
    }
    Layout { root: root.to_path_buf(), index_source, files, problems }
}

pub fn synthetic_index(layout_sprites: impl Iterator<Item = String>) -> IndexData {
    let mut data = IndexData::default();
    for sprite in layout_sprites {
        if sprite != STAGE {
            data.sprites.push(SpriteInfo { name: sprite.clone(), costumes: Vec::new(), sounds: Vec::new() });
            data.sprite_variables.insert(sprite.clone(), Vec::new());
            data.custom_blocks.insert(sprite, Vec::new());
        }
    }
    data
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if let Some(v) = hex_byte(bytes, i) {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest);
    if cfg!(windows) {
        let trimmed = decoded.strip_prefix('/').unwrap_or(&decoded);
        if trimmed.as_bytes().get(1) == Some(&b':') {
            return Some(PathBuf::from(trimmed.replace('/', "\\")));
        }
        return Some(PathBuf::from(decoded.replace('/', "\\")));
    }
    Some(PathBuf::from(decoded))
}

pub fn path_to_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !text.starts_with('/') {
        out.push('/');
    }
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => out.push(b as char),
            b':' if cfg!(windows) => out.push(':'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn file_id(uri: &str) -> Arc<str> {
    match uri_to_path(uri) {
        Some(path) => path_id(&path),
        None => uri.into(),
    }
}

pub fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let resolved = path.canonicalize()?;
    let text = resolved.to_string_lossy();
    Ok(match text.strip_prefix(r"\\?\UNC\") {
        Some(share) if cfg!(windows) => PathBuf::from(format!(r"\\{share}")),
        _ => match text.strip_prefix(r"\\?\") {
            Some(local) if cfg!(windows) => PathBuf::from(local),
            _ => resolved,
        },
    })
}

pub fn canonical_file_id(uri: &str) -> Arc<str> {
    match uri_to_path(uri) {
        Some(path) => path_id(&canonical(&path).unwrap_or(path)),
        None => uri.into(),
    }
}

pub fn path_id(path: &Path) -> Arc<str> {
    let text = path.to_string_lossy();
    if cfg!(windows) { text.to_lowercase().replace('/', "\\").into() } else { text.as_ref().into() }
}

#[derive(Default)]
pub struct HeaderSet {
    pub headers: FxHashMap<String, Arc<str>>,
    pub hash: u64,
}

impl HeaderSet {
    pub fn new(headers: FxHashMap<String, Arc<str>>) -> Arc<HeaderSet> {
        let mut names: Vec<&String> = headers.keys().collect();
        names.sort();
        let mut hasher = xxhash_rust::xxh3::Xxh3::new();
        for name in names {
            hasher.update(name.as_bytes());
            hasher.update(&[0]);
            hasher.update(headers[name].as_bytes());
            hasher.update(&[0]);
        }
        Arc::new(HeaderSet { hash: hasher.digest(), headers })
    }
}

impl Headers for HeaderSet {
    fn header(&self, name: &str) -> Option<Arc<str>> {
        self.headers.get(name).cloned()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Derived {
    pub facts: Arc<FileFacts>,
    pub diags: Vec<Diag>,
}

pub struct Slot {
    pub id: Arc<str>,
    pub uri: Arc<str>,
    pub kind: FileKind,
    pub text: Arc<str>,
    pub version: Option<i32>,
    pub open: bool,
    pub key: u64,
    pub index: Arc<ScratchIndex>,
    pub headers: Arc<HeaderSet>,
    pub analysis: Arc<OnceLock<Arc<Analysis>>>,
    pub derived: Arc<OnceLock<Arc<Derived>>>,
}

pub fn content_key(text: &str, sprite: &str, index: &ScratchIndex, headers: &HeaderSet) -> u64 {
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    hasher.update(text.as_bytes());
    hasher.update(&[0xff]);
    hasher.update(sprite.as_bytes());
    hasher.update(&index.hash.to_le_bytes());
    if text.contains("#include") {
        hasher.update(&headers.hash.to_le_bytes());
    }
    hasher.update(crate::CACHE_FORMAT.as_bytes());
    hasher.digest()
}

impl Slot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: Arc<str>,
        uri: Arc<str>,
        kind: FileKind,
        text: Arc<str>,
        version: Option<i32>,
        open: bool,
        index: &Arc<ScratchIndex>,
        headers: &Arc<HeaderSet>,
    ) -> Slot {
        let key = content_key(&text, kind.sprite().unwrap_or(""), index, headers);
        Slot { id, uri, kind, text, version, open, key, index: index.clone(), headers: headers.clone(), analysis: Arc::default(), derived: Arc::default() }
    }

    pub fn with(&self, uri: Arc<str>, text: Arc<str>, version: Option<i32>, open: bool) -> Slot {
        let mut slot = Slot::new(self.id.clone(), uri, self.kind.clone(), text, version, open, &self.index, &self.headers);
        if slot.key == self.key {
            slot.analysis = self.analysis.clone();
            slot.derived = self.derived.clone();
        }
        slot
    }

    pub fn sprite_context(&self) -> &str {
        match &self.kind {
            FileKind::Sprite(s) => s,
            FileKind::Header(_) => "",
        }
    }

    fn compute(&self) -> Analysis {
        analyze(&self.text, self.sprite_context(), &self.index, &*self.headers)
    }

    pub fn analysis(&self) -> Arc<Analysis> {
        self.analysis.get_or_init(|| Arc::new(self.compute())).clone()
    }

    pub fn derived(&self) -> Arc<Derived> {
        self.derived
            .get_or_init(|| {
                let derive = |analysis: &Analysis| {
                    let all = LintSettings::default();
                    let diags = match self.kind {
                        FileKind::Sprite(_) => local_diagnostics(analysis, &all),
                        FileKind::Header(_) => crate::analyzer::parse_diagnostics(analysis),
                    };
                    Arc::new(Derived { facts: Arc::new(extract_file_facts(self.sprite_context(), analysis)), diags })
                };
                match (self.open, self.analysis.get()) {
                    (_, Some(a)) => derive(a),
                    (true, None) => derive(&self.analysis()),
                    (false, None) => derive(&self.compute()),
                }
            })
            .clone()
    }

    pub fn has_derived(&self) -> bool {
        self.derived.get().is_some()
    }
}

pub type SlotMap = IndexMap<Arc<str>, Arc<Slot>, FxBuildHasher>;

pub use crate::project::MessageCounts;

#[derive(Default)]
pub struct Shared {
    pub stable_counts: std::sync::Mutex<Option<(u64, Arc<MessageCounts>)>>,
}

pub struct Snapshot {
    pub shared: Arc<Shared>,
    pub generation: u64,
    pub root: Arc<PathBuf>,
    pub index: Arc<ScratchIndex>,
    pub index_loaded: bool,
    pub headers: Arc<HeaderSet>,
    pub settings: Arc<Settings>,
    pub slots: Arc<SlotMap>,
    linked: OnceLock<Arc<Linked>>,
}

impl Snapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        shared: Arc<Shared>,
        generation: u64,
        root: Arc<PathBuf>,
        index: Arc<ScratchIndex>,
        index_loaded: bool,
        headers: Arc<HeaderSet>,
        settings: Arc<Settings>,
        slots: Arc<SlotMap>,
    ) -> Snapshot {
        Snapshot { shared, generation, root, index, index_loaded, headers, settings, slots, linked: OnceLock::new() }
    }

    pub fn stable_counts(&self) -> Option<Arc<MessageCounts>> {
        let mut stable = self.shared.stable_counts.lock().unwrap_or_else(|e| e.into_inner());
        let Some(linked) = self.linked() else { return stable.as_ref().map(|(_, c)| c.clone()) };
        if let Some((generation, counts)) = stable.as_ref()
            && (*generation >= self.generation || (linked.files.iter().any(|f| f.has_parse_errors)))
        {
            return Some(counts.clone());
        }
        let counts = Arc::new(crate::project::message_counts(&linked));
        let keep = stable.as_ref().is_some_and(|(_, old)| **old == *counts);
        if !keep {
            *stable = Some((self.generation, counts.clone()));
        } else if let Some((generation, _)) = stable.as_mut() {
            *generation = self.generation;
        }
        stable.as_ref().map(|(_, c)| c.clone())
    }

    pub fn slot(&self, uri: &str) -> Option<&Arc<Slot>> {
        self.slots.get(&*file_id(uri)).or_else(|| self.slots.values().find(|s| &*s.uri == uri))
    }

    pub fn sprite_slot(&self, sprite: &str) -> Option<&Arc<Slot>> {
        self.slots.values().find(|s| s.kind.sprite() == Some(sprite))
    }

    pub fn project_slots(&self) -> impl Iterator<Item = &Arc<Slot>> {
        let index = &self.index;
        self.slots.values().filter(move |s| s.kind.sprite().is_some_and(|sp| index.sprite_names().any(|n| n == sp)))
    }

    pub fn project_ready(&self) -> bool {
        self.index_loaded && self.project_slots().all(|s| s.has_derived())
    }

    pub fn linked(&self) -> Option<Arc<Linked>> {
        if let Some(linked) = self.linked.get() {
            return Some(linked.clone());
        }
        if !self.project_ready() {
            return None;
        }
        Some(self.linked.get_or_init(|| Arc::new(self.link_now())).clone())
    }

    pub fn linked_including(&self, slot: &Slot) -> Option<Arc<Linked>> {
        slot.derived();
        self.linked()
    }

    fn link_now(&self) -> Linked {
        let mut facts = FxHashMap::default();
        for slot in self.project_slots() {
            if let Some(sprite) = slot.kind.sprite() {
                facts.entry(sprite.to_string()).or_insert_with(|| slot.derived().facts.clone());
            }
        }
        let monitored = self.index.data.monitored.iter().map(|(o, k, n)| crate::facts::var_key(o, *k, n)).collect();
        link(&self.index, &facts, &LinkOptions { monitored })
    }

    pub fn sprite_uri(&self, sprite: &str) -> Option<Arc<str>> {
        self.sprite_slot(sprite).map(|s| s.uri.clone())
    }
}
