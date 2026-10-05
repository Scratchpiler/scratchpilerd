use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};

use crate::analyzer::{Access, Analysis, SymKind, symbol_at};
use crate::ast::Span;
use crate::diag::{Category, Diag, Severity};
use crate::facts::{FileFacts, HatInfo, ROUTINE_MESSAGE_PREFIX, Send, SetAccess, UnitKind, VarAccess, VarKey, var_key};
use crate::index::{STAGE, ScratchIndex, VarKind};

const SPECIAL_TARGETS: &[&str] = &["_mouse_", "_random_", "_edge_", "_stage_", "_myself_"];
const SPRITE_ATTRIBUTES: &[&str] = &["x position", "y position", "direction", "costume #", "costume name", "size", "volume"];
const STAGE_ATTRIBUTES: &[&str] = &["backdrop #", "backdrop name", "volume"];
const CLOUD_PREFIX: &str = "☁";

type Ordered<K, V> = IndexMap<K, V, FxBuildHasher>;

pub fn is_routine_message(msg: &str) -> bool {
    msg.starts_with(ROUTINE_MESSAGE_PREFIX)
}

pub fn message_key(name: &str) -> String {
    crate::js::to_upper(name)
}

pub fn display_message(msg: &str) -> &str {
    msg.strip_prefix(ROUTINE_MESSAGE_PREFIX).unwrap_or(msg)
}

pub fn sprite_label(sprite: &str) -> &str {
    if sprite == STAGE { "the Stage" } else { sprite }
}

pub fn is_compiler_variable(name: &str) -> bool {
    name.starts_with("__") || name.starts_with("_scratchpiler_internal_")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UnitRef {
    pub file: u32,
    pub unit: u32,
}

#[derive(Clone, Debug)]
pub struct Site {
    pub sprite: Arc<str>,
    pub unit: UnitRef,
    pub span: Span,
    pub access: Access,
    pub via_attribute: bool,
}

#[derive(Clone, Debug)]
pub struct SendSite {
    pub sprite: Arc<str>,
    pub unit: UnitRef,
    pub send: u32,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ReceiveSite {
    pub sprite: Arc<str>,
    pub unit: UnitRef,
    pub span: Span,
    pub routine: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Variable {
    pub owner: Arc<str>,
    pub kind: VarKind,
    pub name: Arc<str>,
    pub id: String,
    pub accesses: Vec<Site>,
}

#[derive(Clone, Debug, Default)]
pub struct Message {
    pub name: String,
    pub senders: Vec<SendSite>,
    pub receivers: Vec<ReceiveSite>,
}

#[derive(Clone, Debug)]
pub struct ProjectItem {
    pub message: String,
    pub key: VarKey,
}

#[derive(Default)]
pub struct LinkOptions {
    pub monitored: FxHashSet<VarKey>,
}

pub struct Linked {
    pub index: Arc<ScratchIndex>,
    pub sprite_names: Vec<String>,
    pub files: Vec<Arc<FileFacts>>,
    file_by_sprite: FxHashMap<String, u32>,
    pub complete: bool,
    pub clean: bool,
    pub variables: Ordered<VarKey, Variable>,
    pub messages: Ordered<String, Message>,
    pub threads: Vec<UnitRef>,
    pub cloned: FxHashSet<String>,
    pub dynamic_send: bool,
    pub dynamic_clone: bool,
    pub any_asm: bool,
    pub by_sprite: FxHashMap<String, Vec<Diag>>,
    pub project_items: Vec<ProjectItem>,
}

impl Linked {
    pub fn file(&self, sprite: &str) -> Option<&FileFacts> {
        self.file_by_sprite.get(sprite).map(|i| &*self.files[*i as usize])
    }

    pub fn unit(&self, r: UnitRef) -> &crate::facts::Unit {
        &self.files[r.file as usize].units[r.unit as usize]
    }

    pub fn sprite_of(&self, r: UnitRef) -> &str {
        &self.files[r.file as usize].sprite
    }

    fn effective_sends(&self, r: UnitRef) -> impl Iterator<Item = &Send> {
        self.files[r.file as usize].parts(r.unit).flat_map(|u| u.sends.iter())
    }

    pub fn send(&self, site: &SendSite) -> &Send {
        &self.unit(site.unit).sends[site.send as usize]
    }

    pub fn diagnostics_for(&self, sprite: &str) -> &[Diag] {
        self.by_sprite.get(sprite).map(Vec::as_slice).unwrap_or(&[])
    }

    fn is_cloneable(&self, sprite: &str) -> bool {
        sprite != STAGE && (self.dynamic_clone || self.cloned.contains(sprite))
    }
}

pub fn link(index: &Arc<ScratchIndex>, files: &FxHashMap<String, Arc<FileFacts>>, options: &LinkOptions) -> Linked {
    let sprite_names: Vec<String> = index.sprite_names().map(str::to_string).collect();
    let ordered: Vec<Arc<FileFacts>> = sprite_names.iter().filter_map(|n| files.get(n).cloned()).collect();
    let complete = ordered.len() == sprite_names.len();
    let clean = complete && ordered.iter().all(|f| !f.has_parse_errors);

    let mut variables: Ordered<VarKey, Variable> = Ordered::default();
    let mut add_var = |owner: &str, v: &crate::index::VarInfo| {
        variables.insert(
            var_key(owner, v.kind, &v.name),
            Variable { owner: owner.into(), kind: v.kind, name: v.name.as_str().into(), id: v.id.clone(), accesses: Vec::new() },
        );
    };
    for v in &index.data.global_variables {
        add_var(STAGE, v);
    }
    for (sprite, vars) in &index.data.sprite_variables {
        for v in vars {
            add_var(sprite, v);
        }
    }

    let mut messages: Ordered<String, Message> = Ordered::default();
    let mut threads = Vec::new();
    let mut cloned = FxHashSet::default();
    let (mut dynamic_send, mut dynamic_clone, mut any_asm) = (false, false, false);

    for (fi, file) in ordered.iter().enumerate() {
        let sprite: Arc<str> = file.sprite.as_str().into();
        for (ui, unit) in file.units.iter().enumerate() {
            let r = UnitRef { file: fi as u32, unit: ui as u32 };
            for access in &unit.accesses {
                if let Some(v) = variables.get_mut(&access.key) {
                    v.accesses.push(Site { sprite: sprite.clone(), unit: r, span: access.span, access: access.access, via_attribute: false });
                }
            }
            for (si, send) in unit.sends.iter().enumerate() {
                messages.entry(message_key(&send.msg)).or_insert_with(|| Message { name: send.msg.clone(), ..Default::default() }).senders.push(SendSite {
                    sprite: sprite.clone(),
                    unit: r,
                    send: si as u32,
                    span: send.span,
                });
            }
            dynamic_send |= unit.dynamic_send;
            dynamic_clone |= unit.dynamic_clone;
            any_asm |= unit.asm;
            for (name, _) in &unit.clone_targets {
                cloned.insert(if name == "_myself_" { file.sprite.clone() } else { name.clone() });
            }
            for attr in &unit.attribute_refs {
                let owner = if attr.sprite == "_stage_" { STAGE } else { attr.sprite.as_str() };
                if let Some(target) = variables.get_mut(&var_key(owner, VarKind::Variable, &attr.prop))
                    && (owner == STAGE || owner != file.sprite)
                {
                    target.accesses.push(Site { sprite: sprite.clone(), unit: r, span: attr.span, access: Access::Read, via_attribute: true });
                }
            }
            if unit.kind == UnitKind::Script {
                threads.push(r);
            }
        }
        for receive in &file.receives {
            messages.entry(message_key(&receive.msg)).or_insert_with(|| Message { name: receive.msg.clone(), ..Default::default() }).receivers.push(
                ReceiveSite {
                    sprite: sprite.clone(),
                    unit: UnitRef { file: fi as u32, unit: receive.unit },
                    span: receive.span,
                    routine: receive.routine.clone(),
                },
            );
        }
    }

    let file_by_sprite = ordered.iter().enumerate().map(|(i, f)| (f.sprite.clone(), i as u32)).collect();
    let mut linked = Linked {
        index: index.clone(),
        sprite_names,
        files: ordered,
        file_by_sprite,
        complete,
        clean,
        variables,
        messages,
        threads,
        cloned,
        dynamic_send,
        dynamic_clone,
        any_asm,
        by_sprite: FxHashMap::default(),
        project_items: Vec::new(),
    };
    project_diagnostics(&mut linked, options);
    linked
}

struct Pusher<'a> {
    linked: &'a Linked,
    by_sprite: FxHashMap<String, Vec<Diag>>,
    seen: FxHashSet<(String, u32, u32, String)>,
}

impl Pusher<'_> {
    fn push(&mut self, sprite: &str, span: Option<Span>, severity: Severity, message: String, unnecessary: bool) {
        let Some(file) = self.linked.file(sprite) else { return };
        let Some(span) = span else { return };
        if span.line > file.line_count {
            return;
        }
        if !self.seen.insert((sprite.to_string(), span.line, span.col, message.clone())) {
            return;
        }
        let end_line = if span.end_line != 0 { span.end_line } else { span.line };
        let len = if end_line == span.line {
            let end_col = if span.end_col != 0 { span.end_col } else { span.col + 1 };
            (end_col as i64 - span.col as i64).max(1) as u32
        } else {
            1
        };
        let mut diag = Diag::new(span.line, span.col, len, message, severity, Category::Project);
        diag.unnecessary = unnecessary;
        self.by_sprite.entry(sprite.to_string()).or_default().push(diag);
    }
}

fn project_diagnostics(linked: &mut Linked, options: &LinkOptions) {
    let mut project_items = Vec::new();
    let by_sprite = {
        let mut p = Pusher { linked, by_sprite: FxHashMap::default(), seen: FxHashSet::default() };
        event_diagnostics(&mut p);
        variable_diagnostics(&mut p, options, &mut project_items);
        ordering_diagnostics(&mut p);
        wait_cycle_diagnostics(&mut p);
        p.by_sprite
    };
    linked.by_sprite = by_sprite;
    linked.project_items = project_items;
}

fn event_diagnostics(p: &mut Pusher<'_>) {
    let linked = p.linked;
    let sprite_names: FxHashSet<&str> = linked.sprite_names.iter().map(String::as_str).filter(|n| *n != STAGE).collect();
    for message in linked.messages.values() {
        let name = &message.name;
        if is_routine_message(name) {
            continue;
        }
        if message.receivers.is_empty() && linked.clean {
            for send in &message.senders {
                p.push(&send.sprite, Some(send.span), Severity::Warning, format!("Nothing receives \"{name}\""), false);
            }
        }
        if message.senders.is_empty() && !linked.dynamic_send && linked.clean {
            for receive in &message.receivers {
                p.push(&receive.sprite, Some(receive.span), Severity::Warning, format!("Never runs — nothing broadcasts \"{name}\""), false);
            }
        }
    }
    for file in &linked.files {
        for unit in &file.units {
            if let Some(hat) = &unit.hat
                && unit.kind == UnitKind::Script
                && hat.event == "clone"
                && linked.clean
                && !linked.is_cloneable(&file.sprite)
            {
                let message = if file.sprite == STAGE { "The stage has no clones" } else { "Never runs — nothing clones this sprite" };
                p.push(&file.sprite, Some(Span::new(hat.line, hat.col, hat.line, hat.col + 5)), Severity::Warning, message.into(), false);
            }
            for r in &unit.sprite_refs {
                if SPECIAL_TARGETS.contains(&r.name.as_str()) || sprite_names.contains(r.name.as_str()) {
                    continue;
                }
                p.push(&file.sprite, Some(r.span), Severity::Warning, format!("No sprite named \"{}\"", r.name), false);
            }
            for r in &unit.attribute_refs {
                let is_stage = r.sprite == "_stage_";
                if !is_stage && !sprite_names.contains(r.sprite.as_str()) {
                    continue;
                }
                if (if is_stage { STAGE_ATTRIBUTES } else { SPRITE_ATTRIBUTES }).contains(&r.prop.as_str()) {
                    continue;
                }
                if linked.variables.contains_key(&var_key(if is_stage { STAGE } else { &r.sprite }, VarKind::Variable, &r.prop)) {
                    continue;
                }
                let owner = if is_stage { "The stage".to_string() } else { format!("\"{}\"", r.sprite) };
                p.push(&file.sprite, Some(r.span), Severity::Warning, format!("{owner} has no variable \"{}\"", r.prop), false);
            }
        }
    }
}

fn variable_diagnostics(p: &mut Pusher<'_>, options: &LinkOptions, project_items: &mut Vec<ProjectItem>) {
    let linked = p.linked;
    if !linked.clean || linked.any_asm {
        return;
    }
    for (key, variable) in &linked.variables {
        if is_compiler_variable(&variable.name) || variable.name.starts_with(CLOUD_PREFIX) || options.monitored.contains(key) {
            continue;
        }
        let reads: Vec<&Site> = variable.accesses.iter().filter(|a| a.access != Access::Write).collect();
        let writes: Vec<&Site> = variable.accesses.iter().filter(|a| a.access != Access::Read).collect();
        if reads.is_empty() && writes.is_empty() {
            project_items.push(ProjectItem { message: format!("`[{}]` is never used", variable.name), key: key.clone() });
        } else if reads.is_empty() {
            for w in writes {
                p.push(&w.sprite, Some(w.span), Severity::Info, format!("`[{}]` is never read", variable.name), true);
            }
        } else if writes.is_empty() && variable.kind == VarKind::Variable {
            let mut first: Vec<&Site> = Vec::new();
            for r in reads {
                if !first.iter().any(|f| f.sprite == r.sprite) {
                    first.push(r);
                }
            }
            for r in first {
                p.push(&r.sprite, Some(r.span), Severity::Info, format!("`[{}]` is never set — it keeps its saved value", variable.name), false);
            }
        }
    }
}

fn trigger_key(linked: &Linked, r: UnitRef) -> Option<String> {
    let hat = linked.unit(r).hat.as_ref()?;
    match hat.event.as_str() {
        "flag" => Some("flag".into()),
        "key" => Some(format!("key:{}", hat.key.as_deref().unwrap_or("undefined"))),
        "receive" => Some(format!("receive:{}", message_key(hat.msg.as_deref().unwrap_or("")))),
        "backdrop" => Some(format!("backdrop:{}", hat.backdrop.as_deref().unwrap_or("undefined"))),
        "click" => Some(format!("click:{}", linked.sprite_of(r))),
        _ => None,
    }
}

fn trigger_phrase(hat: &HatInfo) -> String {
    match hat.event.as_str() {
        "flag" => "on green flag".into(),
        "key" => format!("on key \"{}\"", hat.key.as_deref().unwrap_or("undefined")),
        "receive" => match &hat.routine {
            Some(routine) => format!("when {routine} launches"),
            None => format!("on \"{}\"", hat.msg.as_deref().unwrap_or("undefined")),
        },
        "backdrop" => format!("on backdrop \"{}\"", hat.backdrop.as_deref().unwrap_or("undefined")),
        "click" => "on click".into(),
        _ => "undefined".into(),
    }
}

struct Segment<'a> {
    thread: UnitRef,
    reads: &'a [VarAccess],
    sets: &'a [SetAccess],
}

fn ordering_diagnostics(p: &mut Pusher<'_>) {
    let linked = p.linked;
    let mut periodic: FxHashSet<String> = FxHashSet::default();
    for message in linked.messages.values() {
        if message.senders.iter().any(|s| linked.send(s).in_loop || linked.unit(s.unit).kind != UnitKind::Script) {
            periodic.insert(message_key(&message.name));
        }
    }
    let mut groups: Ordered<String, Vec<UnitRef>> = Ordered::default();
    for &thread in &linked.threads {
        let Some(key) = trigger_key(linked, thread) else { continue };
        let hat = linked.unit(thread).hat.as_ref().unwrap();
        if hat.event == "receive" {
            let msg = hat.msg.as_deref().unwrap_or("");
            if is_routine_message(msg) || periodic.contains(&message_key(msg)) {
                continue;
            }
        }
        groups.entry(key).or_default().push(thread);
    }
    for group in groups.values() {
        if group.len() < 2 {
            continue;
        }
        let segments: Vec<Segment<'_>> = group
            .iter()
            .map(|t| {
                let seg = linked.unit(*t).segment.as_ref();
                Segment { thread: *t, reads: seg.map(|s| s.reads.as_slice()).unwrap_or(&[]), sets: seg.map(|s| s.sets.as_slice()).unwrap_or(&[]) }
            })
            .collect();
        let mut setters_of: Ordered<&VarKey, Vec<usize>> = Ordered::default();
        for (i, segment) in segments.iter().enumerate() {
            for set in segment.sets {
                setters_of.entry(&set.access.key).or_default().push(i);
            }
        }
        for (ri, reader) in segments.iter().enumerate() {
            let mut reported: FxHashSet<&VarKey> = FxHashSet::default();
            let mut own_sets: FxHashSet<&VarKey> = FxHashSet::default();
            let mut own: Vec<(&VarAccess, bool)> = reader.reads.iter().map(|r| (r, false)).chain(reader.sets.iter().map(|s| (&s.access, true))).collect();
            own.sort_by(|a, b| a.0.span.line.cmp(&b.0.span.line).then(a.0.span.col.cmp(&b.0.span.col)));
            for (access, is_set) in own {
                if is_set {
                    own_sets.insert(&access.key);
                    continue;
                }
                if own_sets.contains(&access.key) || reported.contains(&access.key) {
                    continue;
                }
                let Some(writer) = setters_of.get(&access.key).and_then(|w| w.iter().find(|i| **i != ri)) else { continue };
                reported.insert(&access.key);
                let writer = &segments[*writer];
                let who = if writer.thread.file == reader.thread.file {
                    "another script here".to_string()
                } else {
                    sprite_label(linked.sprite_of(writer.thread)).to_string()
                };
                let phrase = trigger_phrase(linked.unit(reader.thread).hat.as_ref().unwrap());
                p.push(
                    linked.sprite_of(reader.thread),
                    Some(access.span),
                    Severity::Warning,
                    format!("May read `[{}]` before {who} sets it {phrase}", access.key.name),
                    false,
                );
            }
        }
        let mut final_sets: Ordered<&VarKey, Vec<(usize, &SetAccess)>> = Ordered::default();
        for (i, segment) in segments.iter().enumerate() {
            let mut last: Ordered<&VarKey, &SetAccess> = Ordered::default();
            for set in segment.sets {
                last.insert(&set.access.key, set);
            }
            for set in last.values() {
                final_sets.entry(&set.access.key).or_default().push((i, set));
            }
        }
        for (key, sites) in &final_sets {
            let distinct: FxHashSet<&str> = sites.iter().map(|(_, s)| s.value.as_str()).collect();
            if sites.len() < 2 || distinct.len() < 2 {
                continue;
            }
            for (si, set) in sites {
                let Some((oi, _)) = sites.iter().find(|(o, s)| o != si && s.value != set.value) else { continue };
                let segment = &segments[*si];
                let other = &segments[*oi];
                let who = if other.thread.file == segment.thread.file {
                    "another script here".to_string()
                } else {
                    sprite_label(linked.sprite_of(other.thread)).to_string()
                };
                let phrase = trigger_phrase(linked.unit(segment.thread).hat.as_ref().unwrap());
                p.push(
                    linked.sprite_of(segment.thread),
                    Some(set.access.span),
                    Severity::Warning,
                    format!("`[{}]` is also set by {who} {phrase} — the order decides", key.name),
                    false,
                );
            }
        }
    }
}

fn wait_cycle_diagnostics(p: &mut Pusher<'_>) {
    let linked = p.linked;
    let mut waits_from: Ordered<String, Vec<(UnitRef, &Send, String)>> = Ordered::default();
    for &thread in &linked.threads {
        let hat = linked.unit(thread).hat.as_ref().unwrap();
        if hat.event != "receive" {
            continue;
        }
        let from = message_key(hat.msg.as_deref().unwrap_or(""));
        for send in linked.effective_sends(thread) {
            if send.wait {
                waits_from.entry(from.clone()).or_default().push((thread, send, message_key(&send.msg)));
            }
        }
    }
    let mut on_cycle: FxHashSet<String> = FxHashSet::default();
    let mut state: FxHashMap<String, bool> = FxHashMap::default();
    let mut stack: Vec<String> = Vec::new();
    fn visit(
        msg: &str,
        waits: &Ordered<String, Vec<(UnitRef, &Send, String)>>,
        state: &mut FxHashMap<String, bool>,
        stack: &mut Vec<String>,
        on_cycle: &mut FxHashSet<String>,
    ) {
        state.insert(msg.to_string(), true);
        stack.push(msg.to_string());
        for (_, _, target) in waits.get(msg).map(Vec::as_slice).unwrap_or(&[]) {
            match state.get(target) {
                Some(true) => {
                    let start = stack.iter().position(|m| m == target).unwrap_or(0);
                    on_cycle.extend(stack[start..].iter().cloned());
                }
                Some(false) => {}
                None => visit(target, waits, state, stack, on_cycle),
            }
        }
        stack.pop();
        state.insert(msg.to_string(), false);
    }
    for msg in waits_from.keys() {
        if !state.contains_key(msg) {
            visit(msg, &waits_from, &mut state, &mut stack, &mut on_cycle);
        }
    }
    for (msg, waits) in &waits_from {
        if !on_cycle.contains(msg) {
            continue;
        }
        for (thread, send, target) in waits {
            if !on_cycle.contains(target) {
                continue;
            }
            let where_ =
                if target == msg { "restarts its own script".to_string() } else { format!("waits on \"{}\", which waits back", display_message(&send.msg)) };
            p.push(linked.sprite_of(*thread), Some(send.span), Severity::Warning, format!("Never finishes — {where_}"), false);
        }
    }
}

fn within(span: &Span, line: u32, col: u32) -> bool {
    let end_line = if span.end_line != 0 { span.end_line } else { span.line };
    let end_col = if span.end_col != 0 { span.end_col } else { span.col + 1 };
    !(line < span.line || line > end_line || (line == span.line && col < span.col) || (line == end_line && col > end_col))
}

#[derive(Clone, Debug)]
pub enum ProjectHit {
    Message { name: String, span: Span },
    Variable { key: VarKey, span: Span, via_attribute: bool },
}

pub fn project_symbol_at(linked: &Linked, analysis: &Analysis, sprite: &str, line: u32, col: u32) -> Option<ProjectHit> {
    let file = linked.file(sprite)?;
    for unit in &file.units {
        for send in &unit.sends {
            if within(&send.span, line, col) {
                return Some(ProjectHit::Message { name: send.msg.clone(), span: send.span });
            }
        }
        for r in &unit.attribute_refs {
            if !within(&r.span, line, col) {
                continue;
            }
            let owner = if r.sprite == "_stage_" { STAGE } else { r.sprite.as_str() };
            let key = var_key(owner, VarKind::Variable, &r.prop);
            if linked.variables.contains_key(&key) {
                return Some(ProjectHit::Variable { key, span: r.span, via_attribute: true });
            }
        }
    }
    for receive in &file.receives {
        if within(&receive.span, line, col) {
            return Some(ProjectHit::Message { name: receive.msg.clone(), span: receive.span });
        }
    }
    let occ = symbol_at(analysis, line, col)?;
    let sym = analysis.symbol(occ.symbol);
    let kind = match sym.kind {
        SymKind::ProjectVar => VarKind::Variable,
        SymKind::ProjectList => VarKind::List,
        _ => return None,
    };
    let name = sym.name.text();
    let at = analysis
        .occurrences
        .iter()
        .filter(|o| o.span.line == occ.span.line && o.span.col == occ.span.col && !o.is_def)
        .filter_map(|o| {
            let s = analysis.symbol(o.symbol);
            let k = match s.kind {
                SymKind::ProjectVar => VarKind::Variable,
                SymKind::ProjectList => VarKind::List,
                _ => return None,
            };
            Some((k, s.name.text().into_owned(), o.span))
        })
        .next_back();
    let (kind, name, span) = at.unwrap_or((kind, name.into_owned(), occ.span));
    let owner = crate::facts::owner_of(&linked.index, sprite, kind, &name);
    Some(ProjectHit::Variable { key: var_key(&owner, kind, &name), span, via_attribute: false })
}

fn visible_in(linked: &Linked, sprite: &str, span: &Span) -> bool {
    span.line <= linked.file(sprite).map_or(0, |f| f.line_count)
}

pub fn message_sites<'a>(linked: &'a Linked, name: &str) -> (Vec<&'a SendSite>, Vec<&'a ReceiveSite>) {
    let Some(message) = linked.messages.get(&message_key(name)) else { return (Vec::new(), Vec::new()) };
    (
        message.senders.iter().filter(|s| visible_in(linked, &s.sprite, &s.span)).collect(),
        message.receivers.iter().filter(|r| visible_in(linked, &r.sprite, &r.span)).collect(),
    )
}

pub type MessageCounts = Ordered<String, (Vec<Arc<str>>, Vec<Arc<str>>)>;

pub fn message_counts(linked: &Linked) -> MessageCounts {
    let mut counts = Ordered::default();
    for (key, message) in &linked.messages {
        let (senders, receivers) = message_sites(linked, &message.name);
        counts.insert(key.clone(), (senders.iter().map(|s| s.sprite.clone()).collect(), receivers.iter().map(|r| r.sprite.clone()).collect()));
    }
    counts
}

pub fn variable_sites<'a>(linked: &'a Linked, key: &VarKey) -> Vec<&'a Site> {
    linked.variables.get(key).map(|v| v.accesses.iter().filter(|a| visible_in(linked, &a.sprite, &a.span)).collect()).unwrap_or_default()
}

pub fn count_by_sprite<'a>(sprites: impl Iterator<Item = &'a str>) -> String {
    let mut counts: Ordered<&str, usize> = Ordered::default();
    for s in sprites {
        *counts.entry(s).or_default() += 1;
    }
    counts
        .iter()
        .map(|(s, n)| format!("{}{}", if *s == STAGE { "Stage" } else { s }, if *n > 1 { format!(" ({n})") } else { String::new() }))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn describe_variable(linked: &Linked, key: &VarKey) -> (Option<String>, Option<String>) {
    let sites = variable_sites(linked, key);
    let writes: Vec<&str> = sites.iter().filter(|s| s.access != Access::Read).map(|s| &*s.sprite).collect();
    let reads: Vec<&str> = sites.iter().filter(|s| s.access != Access::Write).map(|s| &*s.sprite).collect();
    ((!writes.is_empty()).then(|| count_by_sprite(writes.into_iter())), (!reads.is_empty()).then(|| count_by_sprite(reads.into_iter())))
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ProjectSymbol {
    pub sprite: String,
    pub kind: &'static str,
    pub name: String,
    pub label: String,
    pub line: u32,
    pub col: u32,
}

pub fn project_symbols(linked: &Linked) -> Vec<ProjectSymbol> {
    let mut out = Vec::new();
    for file in &linked.files {
        for unit in &file.units {
            if unit.kind == UnitKind::Orphan || unit.line > file.line_count {
                continue;
            }
            let kind = if unit.kind == UnitKind::Proc {
                "define"
            } else if unit.hat.as_ref().is_some_and(|h| h.routine.is_some()) {
                "routine"
            } else {
                "script"
            };
            out.push(ProjectSymbol {
                sprite: file.sprite.clone(),
                kind,
                name: unit.name.clone().unwrap_or_else(|| unit.label.clone()),
                label: unit.label.clone(),
                line: unit.line,
                col: unit.col,
            });
        }
        for decl in &file.decls {
            out.push(ProjectSymbol {
                sprite: file.sprite.clone(),
                kind: decl.kind.js(),
                name: decl.name.clone(),
                label: decl.name.clone(),
                line: decl.line,
                col: decl.col,
            });
        }
    }
    out
}

pub fn quote(text: &str) -> String {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('{', "{{").replace('}', "}}");
    format!("\"{escaped}\"")
}

pub fn rename_edits(linked: &Linked, target: &ProjectHit, new_name: &str) -> Ordered<String, Vec<(Span, String)>> {
    let mut edits: Ordered<String, Vec<(Span, String)>> = Ordered::default();
    match target {
        ProjectHit::Message { name, .. } => {
            let (senders, receivers) = message_sites(linked, name);
            for s in senders {
                edits.entry(s.sprite.to_string()).or_default().push((s.span, quote(new_name)));
            }
            for r in receivers {
                edits.entry(r.sprite.to_string()).or_default().push((r.span, quote(new_name)));
            }
        }
        ProjectHit::Variable { key, .. } => {
            for site in variable_sites(linked, key) {
                let text = if site.via_attribute { quote(new_name) } else { format!("[{new_name}]") };
                edits.entry(site.sprite.to_string()).or_default().push((site.span, text));
            }
        }
    }
    edits
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct GraphScript {
    pub id: String,
    pub sprite: String,
    pub label: String,
    pub event: String,
    pub line: u32,
    pub col: u32,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct GraphMessage {
    pub id: String,
    pub name: String,
    pub routine: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: &'static str,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct EventGraph {
    pub scripts: Vec<GraphScript>,
    pub messages: Vec<GraphMessage>,
    pub edges: Vec<GraphEdge>,
}

fn key_text(key: &VarKey) -> String {
    format!("{}\u{1}{}\u{1}{}", key.owner, key.kind.js(), key.name)
}

pub fn event_graph(linked: &Linked) -> EventGraph {
    let mut graph = EventGraph::default();
    let mut message_nodes: Ordered<String, GraphMessage> = Ordered::default();
    let mut clone_hats: Ordered<String, Vec<String>> = Ordered::default();
    let visible = |r: UnitRef| linked.unit(r).line <= linked.files[r.file as usize].line_count;
    for &thread in &linked.threads {
        if !visible(thread) {
            continue;
        }
        let unit = linked.unit(thread);
        let file = &linked.files[thread.file as usize];
        let mut reads: Vec<String> = Vec::new();
        let mut writes: Vec<String> = Vec::new();
        for a in file.parts(thread.unit).flat_map(|u| u.accesses.iter()) {
            let k = key_text(&a.key);
            if a.access != Access::Write && !reads.contains(&k) {
                reads.push(k.clone());
            }
            if a.access != Access::Read && !writes.contains(&k) {
                writes.push(k);
            }
        }
        let hat = unit.hat.as_ref().unwrap();
        graph.scripts.push(GraphScript {
            id: unit.key.clone(),
            sprite: file.sprite.clone(),
            label: unit.label.clone(),
            event: hat.event.clone(),
            line: unit.line,
            col: unit.col,
            reads,
            writes,
        });
        if hat.event == "clone" {
            clone_hats.entry(file.sprite.clone()).or_default().push(unit.key.clone());
        }
    }
    let mut message_node = |name: &str| -> String {
        let key = message_key(name);
        message_nodes
            .entry(key.clone())
            .or_insert_with(|| GraphMessage { id: format!("msg:{key}"), name: display_message(name).to_string(), routine: is_routine_message(name) })
            .id
            .clone()
    };
    for &thread in &linked.threads {
        if !visible(thread) {
            continue;
        }
        let unit = linked.unit(thread);
        let file = &linked.files[thread.file as usize];
        let mut sent: Ordered<String, bool> = Ordered::default();
        for send in linked.effective_sends(thread) {
            let node = message_node(&send.msg);
            let wait = sent.get(&node).copied().unwrap_or(false) || send.wait;
            sent.insert(node, wait);
        }
        for (node, wait) in sent {
            graph.edges.push(GraphEdge { from: unit.key.clone(), to: node, kind: if wait { "wait" } else { "send" } });
        }
        let mut targets: Vec<String> = Vec::new();
        for (name, _) in file.parts(thread.unit).flat_map(|u| u.clone_targets.iter()) {
            let target = if name == "_myself_" { file.sprite.clone() } else { name.clone() };
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        for target in targets {
            for hat in clone_hats.get(&target).map(Vec::as_slice).unwrap_or(&[]) {
                graph.edges.push(GraphEdge { from: unit.key.clone(), to: hat.clone(), kind: "clone" });
            }
        }
        let hat = unit.hat.as_ref().unwrap();
        if hat.event == "receive" {
            let from = message_node(hat.msg.as_deref().unwrap_or(""));
            graph.edges.push(GraphEdge { from, to: unit.key.clone(), kind: "receive" });
        }
    }
    graph.messages = message_nodes.into_values().collect();
    graph
}
