mod generated;

use std::sync::OnceLock;

use rustc_hash::{FxHashMap, FxHashSet};

pub use generated::*;

pub struct Sig {
    pub label: &'static str,
    pub params: &'static [(&'static str, &'static str)],
}

pub struct StaticItem {
    pub label: &'static str,
    pub kind: &'static str,
    pub detail: &'static str,
    pub insert_text: &'static str,
    pub documentation: Option<&'static str>,
    pub hat_range: bool,
    pub filter_text: Option<&'static str>,
    pub command: &'static str,
    pub snippet: bool,
}

pub struct AsmParam {
    pub name: &'static str,
    pub kind: &'static str,
    pub value_type: &'static str,
}

struct Tables {
    keywords: FxHashSet<&'static str>,
    call_sigs: FxHashMap<&'static str, &'static str>,
    sig_db: FxHashMap<&'static str, &'static Sig>,
    pen_sig: FxHashMap<&'static str, &'static Sig>,
    asm: FxHashMap<&'static str, &'static [AsmParam]>,
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| Tables {
        keywords: KEYWORDS.iter().copied().collect(),
        call_sigs: CALL_SIGS.iter().copied().collect(),
        sig_db: SIG_DB.iter().map(|(k, v)| (*k, v)).collect(),
        pen_sig: PEN_DOT_SIG.iter().map(|(k, v)| (*k, v)).collect(),
        asm: ASM_OPCODES.iter().map(|(k, v)| (*k, *v)).collect(),
    })
}

pub fn is_keyword(word: &str) -> bool {
    tables().keywords.contains(word)
}

pub fn call_sig(name: &str) -> Option<&'static str> {
    tables().call_sigs.get(name).copied()
}

pub fn sig(name: &str) -> Option<&'static Sig> {
    tables().sig_db.get(name).copied()
}

pub fn pen_sig(name: &str) -> Option<&'static Sig> {
    tables().pen_sig.get(name).copied()
}

pub fn asm_opcode(name: &str) -> Option<&'static [AsmParam]> {
    tables().asm.get(name).copied()
}

pub fn modifier_doc(name: &str) -> Option<(&'static str, &'static str)> {
    MODIFIER_DOCS.iter().find(|(k, _, _)| *k == name).map(|(_, d, doc)| (*d, *doc))
}
