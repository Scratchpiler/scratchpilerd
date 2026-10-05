use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Category {
    Parse,
    Unreachable,
    Orphaned,
    Asm,
    TypeCheck,
    Semantic,
    Smell,
    Project,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Diag {
    pub line: u32,
    pub col: u32,
    pub len: u32,
    pub message: String,
    pub severity: Severity,
    pub category: Category,
    pub whole_line: bool,
    pub unnecessary: bool,
}

impl Diag {
    pub fn new(line: u32, col: u32, len: u32, message: String, severity: Severity, category: Category) -> Diag {
        Diag { line, col, len, message, severity, category, whole_line: false, unnecessary: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LintSettings {
    pub lint_unreachable: bool,
    pub lint_orphaned: bool,
    pub lint_typecheck: bool,
    pub lint_semantic: bool,
    pub lint_smells: bool,
    pub lint_project: bool,
}

impl Default for LintSettings {
    fn default() -> Self {
        LintSettings { lint_unreachable: true, lint_orphaned: true, lint_typecheck: true, lint_semantic: true, lint_smells: true, lint_project: true }
    }
}

impl LintSettings {
    pub fn allows(&self, category: Category) -> bool {
        match category {
            Category::Unreachable => self.lint_unreachable,
            Category::Orphaned => self.lint_orphaned,
            Category::TypeCheck => self.lint_typecheck,
            Category::Semantic => self.lint_semantic,
            Category::Smell => self.lint_smells,
            Category::Project => self.lint_project,
            Category::Parse | Category::Asm => true,
        }
    }
}
