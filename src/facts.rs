use std::sync::Arc;

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::analyzer::{Access, Analysis, SymKind, subtree_max_line};
use crate::ast::*;
use crate::index::{STAGE, ScratchIndex, VarKind};
use crate::js;
use crate::walk::{Node, Val, expr_key, push_nodes, script_nodes};

pub const ROUTINE_MESSAGE_PREFIX: &str = "__sroutine_";

const YIELDING: &[&str] = &[
    "WaitStmt",
    "WaitUntilStmt",
    "GlideStmt",
    "GlideToStmt",
    "SayForStmt",
    "ThinkForStmt",
    "AwaitStmt",
    "BroadcastWaitStmt",
    "AskAndWaitStmt",
    "PlayUntilDoneStmt",
    "SwitchBackdropWaitStmt",
];

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VarKey {
    pub owner: Arc<str>,
    pub kind: VarKind,
    pub name: Arc<str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnitKind {
    Script,
    Proc,
    Orphan,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HatInfo {
    pub event: String,
    pub key: Option<String>,
    pub msg: Option<String>,
    pub backdrop: Option<String>,
    pub sense: Option<String>,
    pub routine: Option<String>,
    pub line: u32,
    pub col: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Send {
    pub msg: String,
    pub wait: bool,
    pub in_loop: bool,
    pub routine: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpriteRef {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttrRef {
    pub prop: String,
    pub sprite: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VarAccess {
    pub key: VarKey,
    pub access: Access,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetAccess {
    pub access: VarAccess,
    pub value: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FirstSegment {
    pub reads: Vec<VarAccess>,
    pub sets: Vec<SetAccess>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Unit {
    pub kind: UnitKind,
    pub key: String,
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub hat: Option<HatInfo>,
    pub name: Option<String>,
    pub numeric_name: bool,
    pub label: String,
    pub warp: bool,
    pub calls: Vec<String>,
    pub sends: Vec<Send>,
    pub dynamic_send: bool,
    pub clone_targets: Vec<(String, Option<Span>)>,
    pub dynamic_clone: bool,
    pub sprite_refs: Vec<SpriteRef>,
    pub attribute_refs: Vec<AttrRef>,
    pub stops: Vec<String>,
    pub deletes_clone: bool,
    pub switches_backdrop: bool,
    pub asm: bool,
    pub accesses: Vec<VarAccess>,
    pub procs: Vec<u32>,
    pub segment: Option<FirstSegment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receive {
    pub msg: String,
    pub routine: Option<String>,
    pub unit: u32,
    pub span: Span,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Decl {
    pub kind: SymKind,
    pub name: String,
    pub line: u32,
    pub col: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileFacts {
    pub sprite: String,
    pub line_count: u32,
    pub has_parse_errors: bool,
    pub units: Vec<Unit>,
    pub receives: Vec<Receive>,
    pub decls: Vec<Decl>,
}

impl FileFacts {
    pub fn parts(&self, unit: u32) -> impl Iterator<Item = &Unit> {
        let u = &self.units[unit as usize];
        std::iter::once(u).chain(u.procs.iter().map(|p| &self.units[*p as usize]))
    }
}

pub fn var_key(owner: &str, kind: VarKind, name: &str) -> VarKey {
    VarKey { owner: owner.into(), kind, name: name.into() }
}

pub fn owner_of(index: &ScratchIndex, sprite: &str, kind: VarKind, name: &str) -> String {
    if sprite == STAGE {
        return STAGE.to_string();
    }
    if index.sprite_vars(sprite).iter().any(|v| v.name == name && v.kind == kind) { sprite.to_string() } else { STAGE.to_string() }
}

fn literal(e: Option<&Expr>) -> Option<&Expr> {
    e.filter(|e| matches!(e, Expr::Str { .. }))
}

fn string_span(e: &Expr) -> Span {
    match e {
        Expr::Str { value, line, col, end_line, end_col } => {
            if *end_line != 0 {
                Span::new(*line, *col, *end_line, *end_col)
            } else {
                Span::new(*line, *col, *line, col + js::utf16_len(value) + 2)
            }
        }
        _ => Span::default(),
    }
}

fn str_value(e: &Expr) -> &str {
    match e {
        Expr::Str { value, .. } => value,
        _ => "",
    }
}

fn sprite_argument(name: &str) -> Option<usize> {
    match name {
        "touching" | "distanceTo" | "xOf" | "yOf" | "directionOf" | "costumeNumOf" | "costumeNameOf" | "sizeOf" | "volumeOf" => Some(0),
        "attributeOf" => Some(1),
        _ => None,
    }
}

pub fn walk_nodes<'a>(node: Node<'a>, in_loop: bool, visit: &mut dyn FnMut(Node<'a>, bool) -> bool) {
    if !visit(node, in_loop) {
        return;
    }
    let loops = in_loop || matches!(node, Node::Stmt(s, _) if s.is_loop());
    let mut children = Vec::new();
    node.fields(&mut |key, value| {
        if !matches!(key, "line" | "col" | "endLine" | "endCol") {
            push_nodes(value, &mut children);
        }
    });
    for child in children {
        walk_nodes(child, loops, visit);
    }
}

pub fn hat_label(hat: &Hat) -> String {
    match &hat.event {
        HatEvent::Flag => "when flag clicked".into(),
        HatEvent::Click => "when clicked".into(),
        HatEvent::Clone => "when I start as a clone".into(),
        HatEvent::Key { key } => format!("when {} key pressed", key.text()),
        HatEvent::Receive { msg, .. } => format!("when I receive \"{}\"", msg.text()),
        HatEvent::Backdrop { backdrop } => format!("when backdrop is \"{}\"", backdrop.text()),
        HatEvent::GreaterThan { sense, .. } => format!("when {} >", sense.to_lowercase()),
        HatEvent::Unknown => "on unknown".into(),
    }
}

fn hat_info(hat: &Hat) -> HatInfo {
    let mut info = HatInfo { event: hat.event_name().into(), key: None, msg: None, backdrop: None, sense: None, routine: None, line: hat.line, col: hat.col };
    match &hat.event {
        HatEvent::Key { key } => info.key = Some(key.text().into_owned()),
        HatEvent::Receive { msg, .. } => info.msg = Some(msg.text().into_owned()),
        HatEvent::Backdrop { backdrop } => info.backdrop = Some(backdrop.text().into_owned()),
        HatEvent::GreaterThan { sense, .. } => info.sense = Some(sense.to_string()),
        _ => {}
    }
    info
}

struct RawUnit<'a> {
    unit: Unit,
    body: &'a [Stmt],
    receive_span: Option<Span>,
}

fn units_of<'a>(ast: &'a Script, sprite: &str) -> Vec<RawUnit<'a>> {
    let mut units = Vec::new();
    for node in script_nodes(ast) {
        let Node::Stmt(block, _) = node else { continue };
        let end_line = block.line.max(subtree_max_line(node));
        let base = |kind, hat, name: Option<String>, numeric_name, label: String, warp| Unit {
            kind,
            key: format!("{sprite}:{}:{}", block.line, block.col),
            line: block.line,
            col: block.col,
            end_line,
            hat,
            name,
            numeric_name,
            label,
            warp,
            calls: Vec::new(),
            sends: Vec::new(),
            dynamic_send: false,
            clone_targets: Vec::new(),
            dynamic_clone: false,
            sprite_refs: Vec::new(),
            attribute_refs: Vec::new(),
            stops: Vec::new(),
            deletes_clone: false,
            switches_backdrop: false,
            asm: false,
            accesses: Vec::new(),
            procs: Vec::new(),
            segment: None,
        };
        match &block.kind {
            StmtKind::On { hat, body } => {
                let receive_span = match &hat.event {
                    HatEvent::Receive { msg_span, .. } => *msg_span,
                    _ => None,
                };
                units.push(RawUnit { unit: base(UnitKind::Script, Some(hat_info(hat)), None, false, hat_label(hat), false), body, receive_span });
            }
            StmtKind::Routine(r) => {
                let hat = HatInfo {
                    event: "receive".into(),
                    key: None,
                    msg: Some(format!("{ROUTINE_MESSAGE_PREFIX}{}", r.name)),
                    backdrop: None,
                    sense: None,
                    routine: Some(r.name.to_string()),
                    line: 0,
                    col: 0,
                };
                let unit = base(UnitKind::Script, Some(hat), Some(r.name.to_string()), false, format!("scratchroutine {}", r.name), false);
                units.push(RawUnit { unit, body: &r.body, receive_span: r.name_span });
            }
            StmtKind::Define(d) => {
                let name = d.name.text().into_owned();
                let label = format!("define {name}");
                let unit = base(UnitKind::Proc, None, Some(name), matches!(d.name, JsVal::Num(_)), label, d.warp || d.returns);
                units.push(RawUnit { unit, body: &d.body, receive_span: None });
            }
            StmtKind::Enum { .. } | StmtKind::Struct { .. } => {}
            _ => {
                let body = block.body().unwrap_or(std::slice::from_ref(block));
                units.push(RawUnit { unit: base(UnitKind::Orphan, None, None, false, "loose blocks".into(), false), body, receive_span: None });
            }
        }
    }
    units
}

fn collect_unit_facts(raw: &mut RawUnit<'_>, define_names: &[String]) {
    let unit = &mut raw.unit;
    for stmt in raw.body {
        walk_nodes(Node::Stmt(stmt, None), false, &mut |node, in_loop| {
            match node {
                Node::Expr(Expr::Call { name, args, .. }) => {
                    if define_names.iter().any(|d| d == &**name) && !unit.calls.iter().any(|c| c == &**name) {
                        unit.calls.push(name.to_string());
                    }
                    if let Some(index) = sprite_argument(name) {
                        let target = args.get(index);
                        if let Some(str) = literal(target) {
                            unit.sprite_refs.push(SpriteRef { name: str_value(str).into(), span: string_span(str) });
                        }
                        if &**name == "attributeOf" {
                            let prop = literal(args.first());
                            let owner = match target {
                                Some(t) => literal(Some(t)).map(str_value),
                                None => Some("_stage_"),
                            };
                            if let (Some(prop), Some(owner)) = (prop, owner) {
                                unit.attribute_refs.push(AttrRef { prop: str_value(prop).into(), sprite: owner.into(), span: string_span(prop) });
                            }
                        }
                    }
                }
                Node::Stmt(s, _) => match &s.kind {
                    StmtKind::Call { name, .. } => {
                        if define_names.iter().any(|d| d == &**name) && !unit.calls.iter().any(|c| c == &**name) {
                            unit.calls.push(name.to_string());
                        }
                    }
                    StmtKind::Simple { kind: kind @ (Simple::Broadcast | Simple::BroadcastWait), .. } => match literal(s.field(F::Msg)) {
                        Some(msg) => unit.sends.push(Send {
                            msg: str_value(msg).into(),
                            wait: *kind == Simple::BroadcastWait,
                            in_loop,
                            routine: None,
                            span: string_span(msg),
                        }),
                        None => unit.dynamic_send = true,
                    },
                    StmtKind::LaunchAwait { wait, name, name_span, .. } => unit.sends.push(Send {
                        msg: format!("{ROUTINE_MESSAGE_PREFIX}{name}"),
                        wait: *wait,
                        in_loop,
                        routine: Some(name.to_string()),
                        span: name_span.unwrap_or(Span::new(s.line, s.col, s.line, s.col + 1)),
                    }),
                    StmtKind::Simple { kind: kind @ (Simple::StopAll | Simple::StopOther), .. } => {
                        let option = kind.stop_option().unwrap_or_default().to_string();
                        if !unit.stops.contains(&option) {
                            unit.stops.push(option);
                        }
                    }
                    StmtKind::Simple { kind: Simple::CreateCloneMyself, .. } => unit.clone_targets.push(("_myself_".into(), None)),
                    StmtKind::Simple { kind: Simple::CreateClone, .. } => match literal(s.field(F::Target)) {
                        None => unit.dynamic_clone = true,
                        Some(str) => {
                            let span = string_span(str);
                            unit.clone_targets.push((str_value(str).into(), Some(span)));
                            unit.sprite_refs.push(SpriteRef { name: str_value(str).into(), span });
                        }
                    },
                    StmtKind::Simple { kind: Simple::DeleteClone, .. } => unit.deletes_clone = true,
                    StmtKind::Asm { .. } => {
                        unit.asm = true;
                        return false;
                    }
                    StmtKind::Simple { kind: Simple::SwitchBackdrop | Simple::SwitchBackdropWait | Simple::NextBackdrop, .. } => unit.switches_backdrop = true,
                    _ => {}
                },
                _ => {}
            }
            if let Node::Stmt(s @ Stmt { kind: StmtKind::Simple { kind: Simple::GoTo | Simple::GlideTo | Simple::PointTowards, .. }, .. }, _) = node
                && let Some(str) = literal(s.field(F::Target))
            {
                unit.sprite_refs.push(SpriteRef { name: str_value(str).into(), span: string_span(str) });
            }
            true
        });
    }
}

type OccurrenceAt = FxHashMap<(u32, u32), VarAccess>;

fn accesses_by_unit(units: &mut [RawUnit<'_>], analysis: &Analysis, sprite: &str, index: &ScratchIndex) -> OccurrenceAt {
    let mut order: Vec<usize> = (0..units.len()).collect();
    order.sort_by_key(|i| units[*i].unit.line);
    let mut occurrence_at = OccurrenceAt::default();
    for occ in &analysis.occurrences {
        let sym = analysis.symbol(occ.symbol);
        let kind = match sym.kind {
            SymKind::ProjectVar => VarKind::Variable,
            SymKind::ProjectList => VarKind::List,
            _ => continue,
        };
        if occ.is_def {
            continue;
        }
        let name = sym.name.text();
        let key = var_key(&owner_of(index, sprite, kind, &name), kind, &name);
        let span = occ.span;
        let access = VarAccess { key, access: occ.access.unwrap_or(Access::Read), span };
        occurrence_at.insert((span.line, span.col), access.clone());
        if let Some(i) = order.iter().rev().find(|i| units[**i].unit.line <= span.line) {
            let unit = &mut units[*i].unit;
            if span.line <= unit.end_line {
                unit.accesses.push(access);
            }
        }
    }
    occurrence_at
}

fn reachable_procs(units: &[RawUnit<'_>], procs: &[(String, u32)], unit: usize) -> Vec<u32> {
    let find = |name: &str| procs.iter().find(|(n, _)| n == name).map(|(_, i)| *i);
    let mut seen: Vec<String> = Vec::new();
    let mut order = Vec::new();
    let mut stack: Vec<String> = units[unit].unit.calls.clone();
    while let Some(name) = stack.pop() {
        if seen.contains(&name) {
            continue;
        }
        let Some(proc_index) = find(&name) else { continue };
        seen.push(name);
        order.push(proc_index);
        stack.extend(units[proc_index as usize].unit.calls.iter().cloned());
    }
    order
}

enum Item<'a> {
    Node(Node<'a>),
    List(Val<'a>),
}

fn push_list<'a>(list: Val<'a>, stack: &mut Vec<Item<'a>>) {
    match list {
        Val::Stmts(l) => stack.extend(l.iter().map(|s| Item::Node(Node::Stmt(s, None)))),
        Val::Exprs(l) => stack.extend(l.iter().map(|e| Item::Node(Node::Expr(e)))),
        Val::Cases(l) => stack.extend(l.iter().map(|c| Item::Node(Node::Case(c)))),
        Val::Entries(l) => stack.extend(l.iter().map(|e| Item::Node(Node::EnumEntry(e)))),
        Val::AsmInsts(l) => stack.extend(l.iter().map(|i| Item::Node(Node::AsmInst(i)))),
        Val::AsmArgs(l) => stack.extend(l.iter().map(|a| Item::Node(Node::AsmArg(a)))),
        _ => {}
    }
}

fn dedupe(reads: &mut Vec<VarAccess>, sets: &mut Vec<SetAccess>) {
    let mut seen: rustc_hash::FxHashSet<(u32, u32, u32, u32)> = Default::default();
    let span_key = |a: &VarAccess| (a.span.line, a.span.col, a.span.end_line, a.span.end_col);
    reads.retain(|a| seen.insert(span_key(a)));
    seen.clear();
    let mut kept: Vec<SetAccess> = Vec::with_capacity(sets.len());
    for set in sets.drain(..).rev() {
        if seen.insert(span_key(&set.access)) {
            kept.push(set);
        }
    }
    kept.reverse();
    *sets = kept;
}

struct Memo {
    reads: Vec<VarAccess>,
    sets: Vec<SetAccess>,
    finished: bool,
    attempted: Vec<u32>,
    on_stack: Vec<u32>,
}

type ProcMemo = FxHashMap<(u32, bool), Vec<Memo>>;

fn on_stack(attempted: &[u32], visiting: &[u32]) -> Vec<u32> {
    let mut hits: Vec<u32> = visiting.iter().copied().filter(|v| attempted.binary_search(v).is_ok()).collect();
    hits.sort_unstable();
    hits.dedup();
    hits
}

struct SegmentRun<'a, 'b> {
    units: &'b [RawUnit<'a>],
    procs: &'b FxHashMap<&'b str, u32>,
    occurrence_at: &'b OccurrenceAt,
    memo: &'b mut ProcMemo,
    scratch: Vec<Item<'a>>,
    visiting: Vec<u32>,
    trail: Vec<u32>,
    warp: bool,
    out: FirstSegment,
}

impl<'a> SegmentRun<'a, '_> {
    fn proc(&self, name: &str) -> Option<u32> {
        self.procs.get(name).copied()
    }

    fn reads_in(&mut self, root: Item<'a>) {
        let mut found = Vec::new();
        let mut stack = std::mem::take(&mut self.scratch);
        stack.clear();
        stack.push(root);
        while let Some(item) = stack.pop() {
            let node = match item {
                Item::List(list) => {
                    push_list(list, &mut stack);
                    continue;
                }
                Item::Node(node) => node,
            };
            match node {
                Node::Expr(Expr::Var { line, col, .. }) => {
                    if let Some(access) = self.occurrence_at.get(&(*line, *col)) {
                        found.push(access.clone());
                    }
                }
                Node::Expr(Expr::Call { name, .. }) => {
                    if let Some(proc) = self.proc(name) {
                        self.scratch = Vec::new();
                        self.call_proc(proc);
                    }
                }
                Node::Span(_) | Node::NamedSpan(_) => continue,
                _ => {}
            }
            node.fields(&mut |key, value| {
                if key == "body" {
                    return;
                }
                match value {
                    Val::Node(n) => stack.push(Item::Node(n)),
                    Val::Stmts(_) | Val::Exprs(_) | Val::Cases(_) | Val::Entries(_) | Val::AsmInsts(_) | Val::AsmArgs(_) => stack.push(Item::List(value)),
                    _ => {}
                }
            });
        }
        self.scratch = stack;
        self.out.reads.extend(found);
    }

    fn call_proc(&mut self, proc: u32) -> bool {
        self.trail.push(proc);
        if self.visiting.contains(&proc) {
            return true;
        }
        let raw = &self.units[proc as usize];
        let saved = self.warp;
        let key = (proc, saved || raw.unit.warp);
        if let Some(memo) = self.memo.get(&key).and_then(|entries| entries.iter().find(|m| m.on_stack == on_stack(&m.attempted, &self.visiting))) {
            self.out.reads.extend_from_slice(&memo.reads);
            self.out.sets.extend(memo.sets.iter().cloned());
            self.trail.extend_from_slice(&memo.attempted);
            return memo.finished;
        }
        let (reads_from, sets_from, trail_from) = (self.out.reads.len(), self.out.sets.len(), self.trail.len());
        self.visiting.push(proc);
        self.warp = key.1;
        let finished = self.run_body(Some(raw.body));
        self.warp = saved;
        self.visiting.pop();
        let mut attempted = self.trail[trail_from..].to_vec();
        attempted.sort_unstable();
        attempted.dedup();
        let mut reads = self.out.reads[reads_from..].to_vec();
        let mut sets = self.out.sets[sets_from..].to_vec();
        dedupe(&mut reads, &mut sets);
        self.out.reads.truncate(reads_from);
        self.out.sets.truncate(sets_from);
        self.out.reads.extend_from_slice(&reads);
        self.out.sets.extend(sets.iter().cloned());
        let on_stack = on_stack(&attempted, &self.visiting);
        self.memo.entry(key).or_default().push(Memo { reads, sets, finished, attempted, on_stack });
        finished
    }

    fn access_at(&self, span: &Option<Span>) -> Option<VarAccess> {
        span.and_then(|s| self.occurrence_at.get(&(s.line, s.col)).cloned())
    }

    fn run_statement(&mut self, stmt: &'a Stmt) -> bool {
        if YIELDING.contains(&stmt.type_name()) {
            self.reads_in(Item::Node(Node::Stmt(stmt, None)));
            return false;
        }
        match &stmt.kind {
            StmtKind::SetVar { value, var_span, .. } => {
                self.reads_in(Item::Node(Node::Expr(value)));
                if let Some(access) = self.access_at(var_span) {
                    self.out.sets.push(SetAccess { access, value: expr_key(value) });
                }
                true
            }
            StmtKind::ChangeVar { value, var_span, .. } => {
                self.reads_in(Item::Node(Node::Expr(value)));
                if let Some(access) = self.access_at(var_span) {
                    self.out.reads.push(access);
                }
                true
            }
            StmtKind::Call { name, args } if self.proc(name).is_some() => {
                self.reads_in(Item::List(Val::Exprs(args)));
                self.call_proc(self.proc(name).unwrap_or_default())
            }
            StmtKind::If { cond, then, alt } => {
                self.reads_in(Item::Node(Node::Expr(cond)));
                let then_finished = self.run_body(Some(then));
                let alt_finished = self.run_body(alt.as_deref());
                then_finished && alt_finished
            }
            _ if stmt.is_loop() => {
                self.reads_in(Item::Node(Node::Stmt(stmt, None)));
                self.warp && self.run_body(stmt.body())
            }
            StmtKind::Match { subject, cases, default_body } => {
                self.reads_in(Item::Node(Node::Expr(subject)));
                let mut all = true;
                for c in cases {
                    all &= self.run_body(Some(&c.body));
                }
                all &= self.run_body(default_body.as_deref());
                all
            }
            _ => {
                self.reads_in(Item::Node(Node::Stmt(stmt, None)));
                true
            }
        }
    }

    fn run_body(&mut self, stmts: Option<&'a [Stmt]>) -> bool {
        for stmt in stmts.unwrap_or_default() {
            if !self.run_statement(stmt) {
                return false;
            }
        }
        true
    }
}

pub fn extract_file_facts(sprite: &str, analysis: &Analysis) -> FileFacts {
    let index = &analysis.index;
    let mut units = units_of(&analysis.ast, sprite);
    let mut procs: Vec<(String, u32)> = Vec::new();
    for (i, raw) in units.iter().enumerate() {
        if raw.unit.kind == UnitKind::Proc && !raw.unit.numeric_name {
            let name = raw.unit.name.clone().unwrap_or_default();
            match procs.iter_mut().find(|(n, _)| *n == name) {
                Some(slot) => slot.1 = i as u32,
                None => procs.push((name, i as u32)),
            }
        }
    }
    let define_names: Vec<String> = procs.iter().map(|(n, _)| n.clone()).collect();
    for raw in units.iter_mut() {
        collect_unit_facts(raw, &define_names);
    }
    let occurrence_at = accesses_by_unit(&mut units, analysis, sprite, index);

    let proc_index: FxHashMap<&str, u32> = procs.iter().map(|(n, i)| (n.as_str(), *i)).collect();
    let mut memo = ProcMemo::default();
    let mut results = Vec::new();
    for i in 0..units.len() {
        if units[i].unit.kind != UnitKind::Script {
            continue;
        }
        let reachable = reachable_procs(&units, &procs, i);
        let mut run = SegmentRun {
            units: &units,
            procs: &proc_index,
            occurrence_at: &occurrence_at,
            memo: &mut memo,
            scratch: Vec::new(),
            visiting: Vec::new(),
            trail: Vec::new(),
            warp: false,
            out: FirstSegment::default(),
        };
        run.run_body(Some(units[i].body));
        let mut segment = run.out;
        dedupe(&mut segment.reads, &mut segment.sets);
        results.push((i, reachable, segment));
    }
    for (i, reachable, segment) in results {
        units[i].unit.procs = reachable;
        units[i].unit.segment = Some(segment);
    }

    let receives = units
        .iter()
        .enumerate()
        .filter(|(_, u)| u.unit.kind == UnitKind::Script && u.unit.hat.as_ref().is_some_and(|h| h.event == "receive"))
        .map(|(i, u)| {
            let hat = u.unit.hat.as_ref().unwrap();
            Receive {
                msg: hat.msg.clone().unwrap_or_default(),
                routine: hat.routine.clone(),
                unit: i as u32,
                span: u.receive_span.unwrap_or(Span::new(u.unit.line, u.unit.col, u.unit.line, u.unit.col + 1)),
            }
        })
        .collect();

    let decls = analysis
        .symbols
        .iter()
        .filter(|s| matches!(s.kind, SymKind::EnumMember | SymKind::Struct) && s.header_origin.is_none())
        .filter_map(|s| s.def_range.map(|d| Decl { kind: s.kind, name: s.name.text().into_owned(), line: d.line, col: d.col }))
        .collect();

    FileFacts {
        sprite: sprite.to_string(),
        line_count: analysis.user_line_count().unwrap_or_else(|| analysis.src.split('\n').count() as u32),
        has_parse_errors: !analysis.parse_errors.is_empty(),
        units: units.into_iter().map(|r| r.unit).collect(),
        receives,
        decls,
    }
}
