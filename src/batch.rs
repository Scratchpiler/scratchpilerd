use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;
use rustc_hash::FxHashMap;

use crate::cache::{self, CacheFile};
use crate::diag::Severity;
use crate::index::ScratchIndex;
use crate::lanes::{LaneSizes, Lanes};
use crate::lsp::load;
use crate::workspace::{FileKind, HeaderSet, Settings, Shared, Slot, SlotMap, Snapshot, path_id, path_to_uri, synthetic_index};

pub struct Report {
    pub files: usize,
    pub cached: usize,
    pub errors: usize,
    pub warnings: usize,
    pub millis: u128,
}

pub fn analyze_project(root: &Path, cache_dir: Option<&Path>, lanes: LaneSizes, print: bool) -> Result<Report, String> {
    let start = Instant::now();
    let root = crate::workspace::canonical(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let cache_path = cache_dir.map(|d| cache::file_for(d, &root));
    let loaded = load(&root, cache_path.as_deref());
    for problem in loaded.layout.problems.iter().chain(loaded.index_error.iter()) {
        eprintln!("scratchpilerd: {problem}");
    }
    let index = loaded
        .index
        .clone()
        .unwrap_or_else(|| ScratchIndex::new(synthetic_index(loaded.layout.files.iter().filter_map(|(_, k)| k.sprite().map(str::to_string))), false));
    let headers: FxHashMap<String, Arc<str>> = loaded
        .texts
        .iter()
        .filter_map(|(_, kind, text)| match kind {
            FileKind::Header(name) => Some((name.to_string(), text.clone())),
            _ => None,
        })
        .collect();
    let headers = HeaderSet::new(headers);
    let mut slots = SlotMap::default();
    let mut cached = 0;
    for (path, kind, text) in &loaded.texts {
        let id = path_id(path);
        let slot = Slot::new(id.clone(), path_to_uri(path).into(), kind.clone(), text.clone(), None, false, &index, &headers);
        if let Some(derived) = loaded.cached.get(&slot.key) {
            let _ = slot.derived.set(derived.clone());
            cached += 1;
        }
        slots.insert(id, Arc::new(slot));
    }
    let snapshot =
        Snapshot::new(Arc::new(Shared::default()), 1, Arc::new(root.clone()), index.clone(), true, headers, Arc::new(Settings::default()), Arc::new(slots));
    let pool = Lanes::new(lanes);
    pool.background.install(|| snapshot.slots.values().collect::<Vec<_>>().par_iter().for_each(|s| drop(s.derived())));
    let linked = snapshot.linked();

    let (mut errors, mut warnings) = (0, 0);
    let mut out = String::new();
    for slot in snapshot.slots.values() {
        let derived = slot.derived();
        let project = slot.kind.sprite().and_then(|s| linked.as_ref().map(|l| l.diagnostics_for(s).to_vec())).unwrap_or_default();
        let path = crate::workspace::uri_to_path(&slot.uri).unwrap_or_default();
        let shown = path.strip_prefix(&root).unwrap_or(&path).display().to_string();
        for d in derived.diags.iter().chain(project.iter()) {
            let label = match d.severity {
                Severity::Error => {
                    errors += 1;
                    "error"
                }
                Severity::Warning => {
                    warnings += 1;
                    "warning"
                }
                Severity::Info => "info",
            };
            if print {
                out.push_str(&format!("{shown}:{}:{}: {label}: {}\n", d.line, d.col, d.message));
            }
        }
    }
    if print {
        print!("{out}");
    }
    if let Some(path) = &cache_path {
        let entries = snapshot.slots.values().filter_map(|s| s.derived.get().map(|d| (s.key, d.clone()))).collect();
        let index = loaded.index_stamp.clone().map(|stamp| (stamp, index.data.clone()));
        let file = CacheFile { format: crate::CACHE_FORMAT.to_string(), index, entries };
        if let Err(e) = cache::store(path, &file) {
            eprintln!("scratchpilerd: could not write cache {}: {e}", path.display());
        }
    }
    Ok(Report { files: snapshot.slots.len(), cached, errors, warnings, millis: start.elapsed().as_millis() })
}
