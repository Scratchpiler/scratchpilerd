use std::sync::Arc;
use std::sync::atomic::Ordering;

use rayon::prelude::*;
use serde_json::{Value, json};

use crate::diag::{Category, Diag, Severity};
use crate::index::STAGE;
use crate::lanes::Lanes;
use crate::lsp::Ctx;
use crate::workspace::{Slot, Snapshot};

pub fn line_lengths(text: &str) -> Vec<u32> {
    text.split('\n').map(crate::js::utf16_len).collect()
}

pub fn to_lsp(d: &Diag, lines: &[u32]) -> Value {
    let line = d.line.min(lines.len() as u32).max(1);
    let end_col = if d.whole_line { lines[line as usize - 1] + 1 } else { d.col + d.len.max(1) };
    let severity = match d.severity {
        Severity::Error => 1,
        Severity::Warning => 2,
        Severity::Info => 3,
    };
    let mut out = json!({
        "range": { "start": { "line": line - 1, "character": d.col - 1 }, "end": { "line": line - 1, "character": end_col - 1 } },
        "severity": severity,
        "source": "scratchpiler",
        "message": d.message,
    });
    if d.unnecessary {
        out["tags"] = json!([1]);
    }
    out
}

pub fn diagnostics_for(snapshot: &Snapshot, slot: &Slot) -> Value {
    let settings = &snapshot.settings.lint;
    let derived = slot.derived();
    let lines = line_lengths(&slot.text);
    let mut out: Vec<Value> = derived.diags.iter().filter(|d| settings.allows(d.category)).map(|d| to_lsp(d, &lines)).collect();
    if let (true, Some(sprite)) = (settings.lint_project, slot.kind.sprite())
        && let Some(linked) = snapshot.linked()
    {
        let current = linked.file(sprite).is_some_and(|_| snapshot.sprite_slot(sprite).is_some_and(|s| s.id == slot.id));
        if current {
            out.extend(linked.diagnostics_for(sprite).iter().map(|d| to_lsp(d, &lines)));
            for item in &linked.project_items {
                if &*item.key.owner == sprite || (sprite == STAGE && &*item.key.owner == STAGE) {
                    let mut d = Diag::new(1, 1, 1, item.message.clone(), Severity::Info, Category::Project);
                    d.unnecessary = true;
                    let mut value = to_lsp(&d, &lines);
                    value["severity"] = json!(4);
                    out.push(value);
                }
            }
        }
    }
    Value::Array(out)
}

pub fn publish_one(snapshot: &Snapshot, ctx: &Ctx, id: &Arc<str>) {
    let Some(slot) = snapshot.slots.get(id) else { return };
    ctx.out.publish(&slot.uri, snapshot.generation, slot.version, diagnostics_for(snapshot, slot));
}

struct Progress<'a> {
    ctx: &'a Ctx,
    token: Option<String>,
    total: usize,
    done: std::sync::atomic::AtomicUsize,
    last: std::sync::atomic::AtomicUsize,
}

impl<'a> Progress<'a> {
    fn begin(ctx: &'a Ctx, total: usize) -> Progress<'a> {
        let token =
            (total >= 8 && ctx.progress_supported.load(Ordering::Relaxed)).then(|| format!("scratchpilerd-analysis-{total}-{:?}", std::time::Instant::now()));
        if let Some(token) = &token {
            ctx.out.request("window/workDoneProgress/create", json!({ "token": token }));
            ctx.out.notify(
                "$/progress",
                json!({ "token": token, "value": { "kind": "begin", "title": "Analyzing project", "percentage": 0, "message": format!("0/{total} files") } }),
            );
        }
        Progress { ctx, token, total, done: Default::default(), last: Default::default() }
    }

    fn tick(&self) {
        let done = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        let Some(token) = &self.token else { return };
        let percentage = done * 100 / self.total.max(1);
        let last = self.last.load(Ordering::Relaxed);
        if (percentage >= last + 5 || done == self.total) && self.last.compare_exchange(last, percentage, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            self.ctx.out.notify(
                "$/progress",
                json!({ "token": token, "value": { "kind": "report", "percentage": percentage, "message": format!("{done}/{} files", self.total) } }),
            );
        }
    }

    fn end(&self) {
        if let Some(token) = &self.token {
            self.ctx.out.notify("$/progress", json!({ "token": token, "value": { "kind": "end" } }));
        }
    }
}

pub fn refresh(snapshot: &Snapshot, ctx: &Ctx, lanes: &Lanes) {
    let mut pending: Vec<&Arc<Slot>> = snapshot.slots.values().filter(|s| !s.has_derived()).collect();
    pending.sort_by_key(|s| !s.open);
    let progress = Progress::begin(ctx, pending.len());
    lanes.background.install(|| {
        pending.par_iter().for_each(|slot| {
            slot.derived();
            progress.tick();
        })
    });
    progress.end();
    snapshot.linked();
    let slots: Vec<&Arc<Slot>> = snapshot.slots.values().collect();
    lanes.background.install(|| {
        slots.par_iter().for_each(|slot| ctx.out.publish(&slot.uri, snapshot.generation, slot.version, diagnostics_for(snapshot, slot)));
    });
}
