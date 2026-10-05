use std::fmt::Write;
use std::sync::Arc;

use crate::analyzer::{analyze, local_diagnostics, semantic_tokens};
use crate::diag::{Diag, LintSettings, Severity};
use crate::index::{IndexData, ScratchIndex};
use crate::preprocess::Headers;
use crate::{js, lexer, parser, walk};

pub fn ast_json(text: &str) -> String {
    let tokens = lexer::tokenize(text, false);
    let (script, errors) = parser::parse(text, &tokens);
    let errors = serde_json::to_string(&errors).unwrap_or_default();
    format!("{{\"ast\":{},\"errors\":{}}}", walk::script_json(&script), errors)
}

pub struct DirHeaders(pub Vec<(String, Arc<str>)>);

impl DirHeaders {
    pub fn load(dir: &str) -> DirHeaders {
        let mut headers = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(".h")
                    && let Ok(text) = std::fs::read_to_string(entry.path())
                {
                    headers.push((name, Arc::from(text)));
                }
            }
        }
        DirHeaders(headers)
    }
}

impl Headers for DirHeaders {
    fn header(&self, name: &str) -> Option<Arc<str>> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, t)| t.clone())
    }
}

pub fn line_lengths(text: &str) -> Vec<u32> {
    text.split('\n').map(js::utf16_len).collect()
}

pub fn marker(d: &Diag, lines: &[u32]) -> (u32, u32, u32) {
    let line = d.line.min(lines.len() as u32).max(1);
    let end_col = if d.whole_line { lines[line as usize - 1] + 1 } else { d.col + d.len.max(1) };
    (line, d.col, end_col)
}

pub fn diag_json(text: &str, index: &Arc<ScratchIndex>, sprite: &str, headers: &dyn Headers) -> String {
    let analysis = analyze(text, sprite, index, headers);
    let lines = line_lengths(text);
    let mut out = String::from("{\"markers\":[");
    for (i, d) in local_diagnostics(&analysis, &LintSettings::default()).iter().enumerate() {
        let (line, col, end_col) = marker(d, &lines);
        let severity = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        };
        let _ = write!(out, "{}[{line},{col},{end_col},\"{severity}\",{}]", if i > 0 { "," } else { "" }, js::json_quote(&d.message));
    }
    out.push_str("],\"tokens\":[");
    for (i, t) in semantic_tokens(&analysis).iter().enumerate() {
        let _ = write!(out, "{}[{},{},{},\"{}\"]", if i > 0 { "," } else { "" }, t.line, t.col, t.length, t.token_type);
    }
    out.push_str("],\"occ\":[");
    for (i, o) in analysis.occurrences.iter().enumerate() {
        let sym = analysis.symbol(o.symbol);
        let access = o.access.map(|a| format!("\"{}\"", a.js())).unwrap_or_else(|| "null".into());
        let _ = write!(
            out,
            "{}[{},{},{},{},\"{}\",{},{},{}]",
            if i > 0 { "," } else { "" },
            o.span.line,
            o.span.col,
            o.span.end_line,
            o.span.end_col,
            sym.kind.js(),
            js::json_quote(&sym.name.text()),
            o.is_def,
            access
        );
    }
    out.push_str("]}");
    out
}

pub fn load_index(path: &str) -> Arc<ScratchIndex> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let data: IndexData = serde_json::from_str(&text).unwrap_or_default();
    ScratchIndex::new(data, true)
}

pub fn project_json(dir: &str) -> String {
    use crate::facts::extract_file_facts;
    use crate::project::*;
    use serde_json::{Value, json};
    let files: indexmap::IndexMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(format!("{dir}/files.json")).unwrap_or_default()).unwrap_or_default();
    let index = load_index(&format!("{dir}/index.json"));
    let mut analyses = Vec::new();
    let mut facts = rustc_hash::FxHashMap::default();
    for sprite in index.sprite_names() {
        if let Some(text) = files.get(sprite) {
            let analysis = analyze(text, sprite, &index, &crate::preprocess::NoHeaders);
            facts.insert(sprite.to_string(), Arc::new(extract_file_facts(sprite, &analysis)));
            analyses.push((sprite.to_string(), analysis));
        }
    }
    let options = LinkOptions { monitored: index.data.monitored.iter().map(|(o, k, n)| crate::facts::var_key(o, *k, n)).collect() };
    let linked = link(&index, &facts, &options);
    let key_text = |k: &crate::facts::VarKey| format!("{}\u{1}{}\u{1}{}", k.owner, k.kind.js(), k.name);
    let severity = |s: Severity| match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    };
    let mut diags = Vec::new();
    for sprite in &linked.sprite_names {
        for d in linked.diagnostics_for(sprite) {
            diags.push(json!([sprite, d.line, d.col, d.len, severity(d.severity), d.message, d.unnecessary]));
        }
    }
    let items: Vec<Value> = linked.project_items.iter().map(|p| json!([p.message, key_text(&p.key)])).collect();
    let counts: Vec<Value> = message_counts(&linked)
        .iter()
        .map(|(k, (s, r))| json!([k, s.iter().map(|x| x.to_string()).collect::<Vec<_>>(), r.iter().map(|x| x.to_string()).collect::<Vec<_>>()]))
        .collect();
    let symbols: Vec<Value> = project_symbols(&linked).iter().map(|s| json!([s.sprite, s.kind, s.name, s.label, s.line, s.col])).collect();
    let graph = event_graph(&linked);
    let g = json!({
        "scripts": graph.scripts.iter().map(|s| json!([s.id, s.sprite, s.label, s.event, s.line, s.col, s.reads, s.writes])).collect::<Vec<_>>(),
        "messages": graph.messages.iter().map(|m| json!([m.id, m.name, m.routine])).collect::<Vec<_>>(),
        "edges": graph.edges.iter().map(|e| json!([e.from, e.to, e.kind])).collect::<Vec<_>>(),
    });
    let mut hits = Vec::new();
    for (sprite, analysis) in &analyses {
        let Some(file) = linked.file(sprite) else { continue };
        let mut n = 0;
        for t in &analysis.tokens {
            if !matches!(t.kind, lexer::Tk::Str | lexer::Tk::Var) || t.line > file.line_count {
                continue;
            }
            n += 1;
            if n > 151 {
                continue;
            }
            let Some(hit) = project_symbol_at(&linked, analysis, sprite, t.line, t.col + 1) else { continue };
            let opt = |v: u32| if v == 0 { Value::Null } else { json!(v) };
            let row = match &hit {
                ProjectHit::Message { name, span } => {
                    let (senders, receivers) = message_sites(&linked, name);
                    let sent = count_by_sprite(senders.iter().map(|s| &*s.sprite));
                    let received = count_by_sprite(receivers.iter().map(|r| &*r.sprite));
                    json!([sprite, t.line, t.col + 1, "message", name, span.line, span.col, opt(span.end_line), opt(span.end_col), [sent, received]])
                }
                ProjectHit::Variable { key, span, .. } => {
                    let (writes, reads) = describe_variable(&linked, key);
                    json!([sprite, t.line, t.col + 1, "variable", key_text(key), span.line, span.col, opt(span.end_line), opt(span.end_col), [writes, reads]])
                }
            };
            hits.push(row);
        }
    }
    let graph_json = format!("{{\"scripts\":{},\"messages\":{},\"edges\":{}}}", g["scripts"], g["messages"], g["edges"]);
    format!(
        "{{\"complete\":{},\"clean\":{},\"diags\":{},\"items\":{},\"counts\":{},\"symbols\":{},\"graph\":{},\"hits\":{}}}",
        linked.complete,
        linked.clean,
        Value::Array(diags),
        Value::Array(items),
        Value::Array(counts),
        Value::Array(symbols),
        graph_json,
        Value::Array(hits)
    )
}

pub fn run(args: &[String]) {
    use std::io::Write;
    let mode = args.first().map(String::as_str).unwrap_or_default();
    let target = args.get(1).cloned().unwrap_or_default();
    if mode == "latency" {
        latency(&target, args.get(2).map(String::as_str).unwrap_or("Stage.sdsl"));
        return;
    }
    if mode == "bench" {
        bench(&target);
        return;
    }
    if mode == "index" {
        let start = std::time::Instant::now();
        let index = crate::loader::load_index(std::path::Path::new(&target)).expect("index");
        eprintln!("loaded in {:?}", start.elapsed());
        println!("{}", serde_json::to_string(&index).unwrap_or_default());
        return;
    }
    let list = std::fs::read_to_string(&target).expect("file list");
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    let mut indexes: std::collections::HashMap<String, Arc<ScratchIndex>> = Default::default();
    let mut headers: std::collections::HashMap<String, DirHeaders> = Default::default();
    for entry in list.lines() {
        let mut parts = entry.split('\t');
        let file = parts.next().unwrap_or_default();
        let index_path = parts.next().unwrap_or_default().to_string();
        let sprite = parts.next().unwrap_or("Sprite1");
        let header_dir = parts.next().unwrap_or_default().to_string();
        let text = || std::fs::read_to_string(file).unwrap_or_default();
        let line = match mode {
            "ast" => ast_json(&text()),
            "project" => project_json(file),
            "editor" => {
                let index = indexes.entry(index_path.clone()).or_insert_with(|| load_index(&index_path)).clone();
                let positions = std::fs::read_to_string(parts.next().unwrap_or_default()).unwrap_or_default();
                editor_json(&text(), &index, sprite, &header_dir, &positions)
            }
            "diag" => {
                let index = indexes.entry(index_path.clone()).or_insert_with(|| load_index(&index_path)).clone();
                let headers = headers.entry(header_dir.clone()).or_insert_with(|| DirHeaders::load(&header_dir));
                diag_json(&text(), &index, sprite, headers)
            }
            other => panic!("unknown debug mode {other}"),
        };
        writeln!(out, "{line}").unwrap();
    }
}

pub fn bench(dir: &str) {
    use std::time::Instant;
    let root = std::path::Path::new(dir);
    let loaded = crate::lsp::load(root, None);
    let index = loaded.index.clone().unwrap_or_else(ScratchIndex::empty);
    let headers = crate::workspace::HeaderSet::new(Default::default());
    let texts: Vec<_> = loaded.texts.iter().filter(|(_, k, _)| k.sprite().is_some()).collect();
    let bytes: usize = texts.iter().map(|(_, _, t)| t.len()).sum();
    let mut times = [0u128; 7];
    let mut facts = rustc_hash::FxHashMap::default();
    for _ in 0..5 {
        for (_, kind, text) in &texts {
            let sprite = kind.sprite().unwrap();
            let t = Instant::now();
            let tokens = lexer::tokenize(text, false);
            times[0] += t.elapsed().as_nanos();
            let t = Instant::now();
            let _ = parser::parse(text, &tokens);
            times[1] += t.elapsed().as_nanos();
            let t = Instant::now();
            let analysis = analyze(text, sprite, &index, &*headers);
            times[2] += t.elapsed().as_nanos();
            let t = Instant::now();
            let _ = local_diagnostics(&analysis, &LintSettings::default());
            times[3] += t.elapsed().as_nanos();
            let t = Instant::now();
            facts.insert(sprite.to_string(), Arc::new(crate::facts::extract_file_facts(sprite, &analysis)));
            times[4] += t.elapsed().as_nanos();
            let t = Instant::now();
            let _ = crate::features::lexical::tokenize(text);
            times[5] += t.elapsed().as_nanos();
        }
        let t = Instant::now();
        let _ = crate::project::link(&index, &facts, &Default::default());
        times[6] += t.elapsed().as_nanos();
    }
    let names = ["tokenize", "parse", "analyze (incl. tokenize+parse)", "local diagnostics", "facts", "lexical highlight", "link project"];
    eprintln!("{} sprite files, {} KB", texts.len(), bytes / 1024);
    for (name, ns) in names.iter().zip(times) {
        let ms = ns as f64 / 5.0 / 1e6;
        eprintln!("  {name:<32} {ms:8.2} ms  {:8.1} MB/s", bytes as f64 / 1e6 / (ms / 1e3));
    }
}

pub fn editor_json(text: &str, index: &Arc<ScratchIndex>, sprite: &str, headers_dir: &str, positions: &str) -> String {
    use crate::workspace::*;
    use serde_json::{Value, json};
    let mut headers = rustc_hash::FxHashMap::default();
    for (name, body) in DirHeaders::load(headers_dir).0 {
        headers.insert(name, body);
    }
    let headers = HeaderSet::new(headers);
    let kind = FileKind::Sprite(sprite.into());
    let uri: Arc<str> = "file:///debug/test.sdsl".into();
    let id = file_id(&uri);
    let slot = Arc::new(Slot::new(id.clone(), uri.clone(), kind, text.into(), Some(1), true, index, &headers));
    let mut slots = SlotMap::default();
    slots.insert(id, slot.clone());
    let snapshot = Snapshot::new(
        Arc::new(Shared::default()),
        1,
        Arc::new(std::path::PathBuf::new()),
        index.clone(),
        true,
        headers,
        Arc::new(Settings::default()),
        Arc::new(slots),
    );
    let positions: Vec<(u32, u32, Option<String>)> = serde_json::from_str(positions).unwrap_or_default();
    let mut rows = Vec::new();
    for (line, col, trigger) in positions {
        let items = crate::features::completion::debug_items(&snapshot, &slot, line, col, trigger.as_deref());
        let hover = crate::features::hover::language_hover(&slot, line, col).map(|(r, c)| json!([r, c])).unwrap_or(Value::Null);
        let params = json!({ "textDocument": { "uri": &*uri }, "position": { "line": line - 1, "character": col - 1 } });
        let sig = crate::features::signature::signature_help(&snapshot, &params).unwrap_or(Value::Null);
        rows.push(json!([items, hover, sig]));
    }
    serde_json::to_string(&rows).unwrap_or_default()
}

pub fn latency(dir: &str, file: &str) {
    use std::time::Instant;
    let root = std::path::Path::new(dir).canonicalize().unwrap_or_default();
    let loaded = crate::lsp::load(&root, None);
    let index = loaded.index.clone().unwrap_or_else(ScratchIndex::empty);
    let headers = crate::workspace::HeaderSet::new(Default::default());
    let mut slots = crate::workspace::SlotMap::default();
    let mut target = None;
    for (path, kind, text) in &loaded.texts {
        let id = crate::workspace::path_id(path);
        let uri: Arc<str> = crate::workspace::path_to_uri(path).into();
        let open = path.file_name().is_some_and(|n| n == file);
        if open {
            target = Some((uri.clone(), text.clone()));
        }
        slots.insert(id.clone(), Arc::new(crate::workspace::Slot::new(id, uri, kind.clone(), text.clone(), Some(1), open, &index, &headers)));
    }
    let snapshot = crate::workspace::Snapshot::new(
        Arc::new(crate::workspace::Shared::default()),
        1,
        Arc::new(root),
        index,
        true,
        headers,
        Arc::new(crate::workspace::Settings::default()),
        Arc::new(slots),
    );
    let (uri, text) = target.expect("file in project");
    let t = Instant::now();
    snapshot.slots.values().for_each(|s| drop(s.derived()));
    snapshot.linked();
    eprintln!("first full pass (single thread): {:.2} ms", t.elapsed().as_secs_f64() * 1e3);
    let lines: Vec<&str> = text.split('\n').collect();
    for method in [
        "textDocument/hover",
        "textDocument/completion",
        "textDocument/definition",
        "textDocument/references",
        "textDocument/semanticTokens/full",
        "textDocument/inlayHint",
        "textDocument/signatureHelp",
        "textDocument/documentSymbol",
    ] {
        let mut times = Vec::new();
        let mut bytes = 0;
        for i in 0..200 {
            let line = lines.len() * (i % 50) / 50;
            let params = serde_json::json!({
                "textDocument": { "uri": &*uri },
                "position": { "line": line, "character": lines[line].len().min(10) },
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": lines.len(), "character": 0 } },
                "context": { "includeDeclaration": true },
            });
            let t = Instant::now();
            let encoded = match crate::features::request(&snapshot, method, &params) {
                Ok(crate::features::Body::Json(v)) => serde_json::to_vec(&v).unwrap_or_default(),
                Ok(crate::features::Body::Raw(raw)) => raw.into_bytes(),
                Err(_) => Vec::new(),
            };
            times.push(t.elapsed().as_secs_f64() * 1e3);
            bytes += encoded.len();
        }
        times.sort_by(|a, b| a.total_cmp(b));
        eprintln!("  {method:<36} p50 {:7.3} ms  p99 {:7.3} ms  {:>7} bytes", times[100], times[198], bytes / 200);
    }
}
