use std::borrow::Cow;

use crate::js;

#[derive(Clone, Debug, PartialEq)]
pub enum JsVal {
    Str(Box<str>),
    Num(f64),
}

impl JsVal {
    pub fn text(&self) -> Cow<'_, str> {
        match self {
            JsVal::Str(s) => Cow::Borrowed(s),
            JsVal::Num(n) => Cow::Owned(js::number_to_string(*n)),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsVal::Str(s) => Some(s),
            JsVal::Num(_) => None,
        }
    }

    pub fn is(&self, word: &str) -> bool {
        matches!(self, JsVal::Str(s) if &**s == word)
    }

    pub fn js_len(&self) -> Option<u32> {
        match self {
            JsVal::Str(s) => Some(js::utf16_len(s)),
            JsVal::Num(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl Span {
    pub fn new(line: u32, col: u32, end_line: u32, end_col: u32) -> Span {
        Span { line, col, end_line, end_col }
    }
}

#[derive(Clone, Debug)]
pub struct NamedSpan {
    pub name: JsVal,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Lt,
    Gt,
    Eq,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

impl BinOp {
    pub fn js(self) -> &'static str {
        match self {
            BinOp::Or => "or",
            BinOp::And => "and",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Eq => "=",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "mod",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Num { value: f64, line: u32, col: u32 },
    Str { value: Box<str>, line: u32, col: u32, end_line: u32, end_col: u32 },
    Hex { value: Box<str>, line: u32, col: u32 },
    Bool { value: bool, line: u32, col: u32 },
    Var { name: Box<str>, line: u32, col: u32, end_line: u32, end_col: u32 },
    Reporter { name: Box<str>, line: u32, col: u32 },
    Call { name: Box<str>, args: Vec<Expr>, line: u32, col: u32 },
    Member { object: Box<Expr>, method: Box<str>, args: Vec<Expr>, line: u32, col: u32 },
    Bin { op: BinOp, left: Box<Expr>, right: Box<Expr> },
    Unary { op: UnOp, operand: Box<Expr> },
    Ternary { cond: Box<Expr>, then: Box<Expr>, alt: Box<Expr>, line: u32, col: u32 },
    Deref { addr: Box<Expr>, line: u32, col: u32 },
    Addr { var_name: Box<str>, line: u32, col: u32, var_span: Span },
}

impl Expr {
    pub fn type_name(&self) -> &'static str {
        match self {
            Expr::Num { .. } => "Num",
            Expr::Str { .. } => "Str",
            Expr::Hex { .. } => "Hex",
            Expr::Bool { .. } => "Bool",
            Expr::Var { .. } => "Var",
            Expr::Reporter { .. } => "Reporter",
            Expr::Call { .. } => "CallExpr",
            Expr::Member { .. } => "MemberCall",
            Expr::Bin { .. } => "BinOp",
            Expr::Unary { .. } => "UnaryOp",
            Expr::Ternary { .. } => "TernaryExpr",
            Expr::Deref { .. } => "DerefExpr",
            Expr::Addr { .. } => "AddrExpr",
        }
    }

    pub fn pos(&self) -> (u32, u32) {
        match self {
            Expr::Num { line, col, .. }
            | Expr::Str { line, col, .. }
            | Expr::Hex { line, col, .. }
            | Expr::Bool { line, col, .. }
            | Expr::Var { line, col, .. }
            | Expr::Reporter { line, col, .. }
            | Expr::Call { line, col, .. }
            | Expr::Member { line, col, .. }
            | Expr::Ternary { line, col, .. }
            | Expr::Deref { line, col, .. }
            | Expr::Addr { line, col, .. } => (*line, *col),
            Expr::Bin { .. } | Expr::Unary { .. } => (0, 0),
        }
    }

    pub fn end(&self) -> (u32, u32) {
        match self {
            Expr::Str { end_line, end_col, .. } | Expr::Var { end_line, end_col, .. } => (*end_line, *end_col),
            _ => (0, 0),
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Expr::Var { name, .. } | Expr::Reporter { name, .. } | Expr::Call { name, .. } => Some(name),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum F {
    Steps,
    Degrees,
    X,
    Y,
    Target,
    Secs,
    Value,
    Msg,
    Name,
    Sound,
    Duration,
    Layers,
    Effect,
    Amount,
    Question,
    Mode,
    Style,
    Color,
    Param,
    Addr,
    Item,
    Index,
    ValueExpr,
    CountExpr,
    ClearFirst,
}

impl F {
    pub fn js(self) -> &'static str {
        match self {
            F::Steps => "steps",
            F::Degrees => "degrees",
            F::X => "x",
            F::Y => "y",
            F::Target => "target",
            F::Secs => "secs",
            F::Value => "value",
            F::Msg => "msg",
            F::Name => "name",
            F::Sound => "sound",
            F::Duration => "duration",
            F::Layers => "layers",
            F::Effect => "effect",
            F::Amount => "amount",
            F::Question => "question",
            F::Mode => "mode",
            F::Style => "style",
            F::Color => "color",
            F::Param => "param",
            F::Addr => "addr",
            F::Item => "item",
            F::Index => "index",
            F::ValueExpr => "valueExpr",
            F::CountExpr => "countExpr",
            F::ClearFirst => "clearFirst",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Simple {
    Move,
    TurnRight,
    TurnLeft,
    GoToXY,
    GoTo,
    GlideTo,
    Glide,
    Bounce,
    SetX,
    SetY,
    ChangeX,
    ChangeY,
    Say,
    SayFor,
    Think,
    ThinkFor,
    SwitchCostume,
    SwitchBackdrop,
    NextCostume,
    NextBackdrop,
    SetSize,
    ChangeSize,
    Show,
    Hide,
    ClearEffects,
    Play,
    PlayUntilDone,
    StopSounds,
    Broadcast,
    BroadcastWait,
    Wait,
    StopAll,
    StopThis,
    StopOther,
    CreateCloneMyself,
    CreateClone,
    DeleteClone,
    SetDirection,
    PointTowards,
    MoveForwardLayers,
    MoveBackwardLayers,
    GoToFront,
    GoToBack,
    SetEffect,
    ChangeEffect,
    SetVolume,
    ChangeVolume,
    AskAndWait,
    ResetTimer,
    SetDragMode,
    PenDown,
    PenUp,
    PenClear,
    PenStamp,
    SetPenColor,
    SetPenSize,
    ChangePenSize,
    SetPenColorParam,
    ChangePenColorParam,
    SetRotationStyle,
    SwitchBackdropWait,
    SetSoundEffect,
    ChangeSoundEffect,
    ClearSoundEffects,
    DerefSet,
    CheckCancel,
}

impl Simple {
    pub fn type_name(self) -> &'static str {
        use Simple::*;
        match self {
            Move => "MoveStmt",
            TurnRight | TurnLeft => "TurnStmt",
            GoToXY => "GoToXYStmt",
            GoTo => "GoToStmt",
            GlideTo => "GlideToStmt",
            Glide => "GlideStmt",
            Bounce => "BounceStmt",
            SetX => "SetXStmt",
            SetY => "SetYStmt",
            ChangeX => "ChangeXStmt",
            ChangeY => "ChangeYStmt",
            Say => "SayStmt",
            SayFor => "SayForStmt",
            Think => "ThinkStmt",
            ThinkFor => "ThinkForStmt",
            SwitchCostume => "SwitchCostumeStmt",
            SwitchBackdrop => "SwitchBackdropStmt",
            NextCostume => "NextCostumeStmt",
            NextBackdrop => "NextBackdropStmt",
            SetSize => "SetSizeStmt",
            ChangeSize => "ChangeSizeStmt",
            Show => "ShowStmt",
            Hide => "HideStmt",
            ClearEffects => "ClearEffectsStmt",
            Play => "PlayStmt",
            PlayUntilDone => "PlayUntilDoneStmt",
            StopSounds => "StopSoundsStmt",
            Broadcast => "BroadcastStmt",
            BroadcastWait => "BroadcastWaitStmt",
            Wait => "WaitStmt",
            StopAll | StopThis | StopOther => "StopStmt",
            CreateCloneMyself | CreateClone => "CreateCloneStmt",
            DeleteClone => "DeleteCloneStmt",
            SetDirection => "SetDirectionStmt",
            PointTowards => "PointTowardsStmt",
            MoveForwardLayers => "MoveForwardLayersStmt",
            MoveBackwardLayers => "MoveBackwardLayersStmt",
            GoToFront => "GoToFrontStmt",
            GoToBack => "GoToBackStmt",
            SetEffect => "SetEffectStmt",
            ChangeEffect => "ChangeEffectStmt",
            SetVolume => "SetVolumeStmt",
            ChangeVolume => "ChangeVolumeStmt",
            AskAndWait => "AskAndWaitStmt",
            ResetTimer => "ResetTimerStmt",
            SetDragMode => "SetDragModeStmt",
            PenDown => "PenDownStmt",
            PenUp => "PenUpStmt",
            PenClear => "PenClearStmt",
            PenStamp => "PenStampStmt",
            SetPenColor => "SetPenColorStmt",
            SetPenSize => "SetPenSizeStmt",
            ChangePenSize => "ChangePenSizeStmt",
            SetPenColorParam => "SetPenColorParamStmt",
            ChangePenColorParam => "ChangePenColorParamStmt",
            SetRotationStyle => "SetRotationStyleStmt",
            SwitchBackdropWait => "SwitchBackdropWaitStmt",
            SetSoundEffect => "SetSoundEffectStmt",
            ChangeSoundEffect => "ChangeSoundEffectStmt",
            ClearSoundEffects => "ClearSoundEffectsStmt",
            DerefSet => "DerefSetStmt",
            CheckCancel => "CheckCancelStmt",
        }
    }

    pub fn stop_option(self) -> Option<&'static str> {
        match self {
            Simple::StopAll => Some("all"),
            Simple::StopThis => Some("this script"),
            Simple::StopOther => Some("other scripts in sprite"),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListOp {
    Add,
    Delete,
    Insert,
    Replace,
    DeleteAll,
    Populate,
}

impl ListOp {
    pub fn type_name(self) -> &'static str {
        match self {
            ListOp::Add => "ListAddStmt",
            ListOp::Delete => "ListDeleteStmt",
            ListOp::Insert => "ListInsertStmt",
            ListOp::Replace => "ListReplaceStmt",
            ListOp::DeleteAll => "ListDeleteAllStmt",
            ListOp::Populate => "PopulateListStmt",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    ShowVar,
    HideVar,
    ShowList,
    HideList,
}

impl Visibility {
    pub fn type_name(self) -> &'static str {
        match self {
            Visibility::ShowVar => "ShowVarStmt",
            Visibility::HideVar => "HideVarStmt",
            Visibility::ShowList => "ShowListStmt",
            Visibility::HideList => "HideListStmt",
        }
    }

    pub fn is_list(self) -> bool {
        matches!(self, Visibility::ShowList | Visibility::HideList)
    }
}

#[derive(Clone, Debug)]
pub enum HatEvent {
    Flag,
    Click,
    Clone,
    Key { key: JsVal },
    Receive { msg: JsVal, msg_span: Option<Span> },
    Backdrop { backdrop: JsVal },
    GreaterThan { sense: &'static str, threshold: Box<Expr> },
    Unknown,
}

#[derive(Clone, Debug)]
pub struct Hat {
    pub event: HatEvent,
    pub line: u32,
    pub col: u32,
}

impl Hat {
    pub fn event_name(&self) -> &'static str {
        match self.event {
            HatEvent::Flag => "flag",
            HatEvent::Click => "click",
            HatEvent::Clone => "clone",
            HatEvent::Key { .. } => "key",
            HatEvent::Receive { .. } => "receive",
            HatEvent::Backdrop { .. } => "backdrop",
            HatEvent::GreaterThan { .. } => "greaterThan",
            HatEvent::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DefineBlock {
    pub name: JsVal,
    pub params: Vec<JsVal>,
    pub body: Vec<Stmt>,
    pub returns: bool,
    pub warp: bool,
    pub noinline: bool,
    pub name_end_line: u32,
    pub name_end_col: u32,
    pub param_spans: Vec<NamedSpan>,
}

#[derive(Clone, Debug)]
pub struct Routine {
    pub name: Box<str>,
    pub params: Vec<JsVal>,
    pub body: Vec<Stmt>,
    pub name_span: Option<Span>,
    pub param_spans: Vec<NamedSpan>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum EnumValue {
    Num(f64),
    Str(Box<str>),
}

#[derive(Clone, Debug)]
pub struct EnumEntry {
    pub name: Box<str>,
    pub value: EnumValue,
    pub value_line: u32,
    pub value_col: u32,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Case {
    pub values: Vec<Expr>,
    pub body: Vec<Stmt>,
    pub line: u32,
    pub col: u32,
}

#[derive(Clone, Debug)]
pub enum AsmArg {
    Str(Box<str>),
    Num(f64),
    Bool(bool),
    Reg(Box<str>),
}

#[derive(Clone, Debug)]
pub struct AsmInst {
    pub opcode: Box<str>,
    pub args: Vec<(AsmArg, u32, u32)>,
    pub line: u32,
    pub col: u32,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Define(Box<DefineBlock>),
    On { hat: Box<Hat>, body: Vec<Stmt> },
    Routine(Box<Routine>),
    Struct { name: Box<str>, fields: Vec<Box<str>>, name_span: Option<Span>, field_spans: Vec<NamedSpan> },
    Enum { entries: Vec<EnumEntry> },
    Orphaned { body: Vec<Stmt> },
    If { cond: Expr, then: Vec<Stmt>, alt: Option<Vec<Stmt>> },
    Repeat { count: Expr, body: Vec<Stmt>, nounroll: bool },
    RepeatUntil { cond: Expr, body: Vec<Stmt> },
    Forever { body: Vec<Stmt> },
    WaitUntil { cond: Expr },
    While { cond: Expr, body: Vec<Stmt> },
    For { var_name: Box<str>, from: Expr, to: Expr, body: Vec<Stmt>, nounroll: bool, var_span: Option<Span> },
    PyFor { var_name: Box<str>, list_name: Box<str>, body: Vec<Stmt>, var_span: Option<Span>, list_span: Option<Span> },
    LaunchAwait { wait: bool, name: Box<str>, args: Vec<Expr>, name_span: Option<Span> },
    Cancel { name: Box<str>, name_span: Option<Span> },
    Breakpoint,
    Asm { unsafe_mode: bool, statements: Vec<AsmInst> },
    Return { value: Option<Expr> },
    Match { subject: Expr, cases: Vec<Case>, default_body: Option<Vec<Stmt>> },
    DoWhile { cond: Expr, body: Vec<Stmt> },
    Break,
    Continue,
    MemberCall { object: Expr, method: Box<str>, args: Vec<Expr> },
    Unknown { value: JsVal },
    SetVar { var_name: Box<str>, value: Expr, var_span: Option<Span> },
    ChangeVar { var_name: Box<str>, value: Expr, var_span: Option<Span> },
    Call { name: Box<str>, args: Vec<Expr> },
    RawKeyword { value: Box<str> },
    Simple { kind: Simple, fields: Vec<(F, Expr)> },
    List { op: ListOp, list_name: JsVal, list_span: Option<Span>, fields: Vec<(F, Expr)> },
    Visibility { kind: Visibility, name: JsVal, name_span: Option<Span> },
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub line: u32,
    pub col: u32,
}

impl Stmt {
    pub fn type_name(&self) -> &'static str {
        match &self.kind {
            StmtKind::Define(_) => "DefineBlock",
            StmtKind::On { .. } => "OnBlock",
            StmtKind::Routine(_) => "ScratchroutineStmt",
            StmtKind::Struct { .. } => "StructDecl",
            StmtKind::Enum { .. } => "EnumDecl",
            StmtKind::Orphaned { .. } => "OrphanedBlock",
            StmtKind::If { .. } => "IfStmt",
            StmtKind::Repeat { .. } => "RepeatStmt",
            StmtKind::RepeatUntil { .. } => "RepeatUntilStmt",
            StmtKind::Forever { .. } => "ForeverStmt",
            StmtKind::WaitUntil { .. } => "WaitUntilStmt",
            StmtKind::While { .. } => "WhileStmt",
            StmtKind::For { .. } => "ForStmt",
            StmtKind::PyFor { .. } => "PyForStmt",
            StmtKind::LaunchAwait { wait: false, .. } => "LaunchStmt",
            StmtKind::LaunchAwait { wait: true, .. } => "AwaitStmt",
            StmtKind::Cancel { .. } => "CancelStmt",
            StmtKind::Breakpoint => "BreakpointStmt",
            StmtKind::Asm { .. } => "AsmStmt",
            StmtKind::Return { .. } => "ReturnStmt",
            StmtKind::Match { .. } => "MatchStmt",
            StmtKind::DoWhile { .. } => "DoWhileStmt",
            StmtKind::Break => "BreakStmt",
            StmtKind::Continue => "ContinueStmt",
            StmtKind::MemberCall { .. } => "MemberCallStmt",
            StmtKind::Unknown { .. } => "UnknownStmt",
            StmtKind::SetVar { .. } => "SetVarStmt",
            StmtKind::ChangeVar { .. } => "ChangeVarStmt",
            StmtKind::Call { .. } => "CallStmt",
            StmtKind::RawKeyword { .. } => "RawKeyword",
            StmtKind::Simple { kind, .. } => kind.type_name(),
            StmtKind::List { op, .. } => op.type_name(),
            StmtKind::Visibility { kind, .. } => kind.type_name(),
        }
    }

    pub fn body(&self) -> Option<&[Stmt]> {
        match &self.kind {
            StmtKind::Define(d) => Some(&d.body),
            StmtKind::On { body, .. }
            | StmtKind::Orphaned { body }
            | StmtKind::Repeat { body, .. }
            | StmtKind::RepeatUntil { body, .. }
            | StmtKind::Forever { body }
            | StmtKind::While { body, .. }
            | StmtKind::For { body, .. }
            | StmtKind::PyFor { body, .. }
            | StmtKind::DoWhile { body, .. } => Some(body),
            StmtKind::Routine(r) => Some(&r.body),
            _ => None,
        }
    }

    pub fn field(&self, wanted: F) -> Option<&Expr> {
        match &self.kind {
            StmtKind::Simple { fields, .. } | StmtKind::List { fields, .. } => fields.iter().find(|(f, _)| *f == wanted).map(|(_, e)| e),
            StmtKind::SetVar { value, .. } | StmtKind::ChangeVar { value, .. } if wanted == F::Value => Some(value),
            StmtKind::Return { value } if wanted == F::Value => value.as_ref(),
            _ => None,
        }
    }

    pub fn is_loop(&self) -> bool {
        matches!(
            self.kind,
            StmtKind::Forever { .. }
                | StmtKind::Repeat { .. }
                | StmtKind::RepeatUntil { .. }
                | StmtKind::While { .. }
                | StmtKind::For { .. }
                | StmtKind::PyFor { .. }
                | StmtKind::DoWhile { .. }
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct Script {
    pub blocks: Vec<Stmt>,
    pub spans: Vec<Span>,
}
