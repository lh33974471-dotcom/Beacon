#[derive(Debug, Clone)]
pub enum Stmt {
    Include(String),
    VarDecl(Vec<(String, Option<Expr>)>),
    VarDestructure(Vec<String>, Expr),
    VarObjectDestructure(Vec<String>, Expr),
    ConstDecl(Vec<(String, Option<Expr>)>),
    Function(FunctionDecl),
    Class(ClassDecl),
    Event(EventDecl),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    For(Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Box<Stmt>),
    ForIn(ForInHead, Expr, Box<Stmt>),
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    Switch(Expr, Vec<SwitchCase>),
    Try(Vec<Stmt>, Option<(String, Vec<Stmt>)>, Option<Vec<Stmt>>),
    Throw(Expr),
    Assert(Expr, Option<Expr>),
    Return(Option<Expr>),
    Break,
    Continue,
    Block(Vec<Stmt>),
    Expr(Expr),
    Empty,
}

#[derive(Debug, Clone)]
pub enum ForInHead {
    One(String),
    Pair(String, String),
    Triple(String, String, String),
}

#[derive(Debug, Clone)]
pub struct FunctionDecl {
    pub path: Vec<String>,
    pub params: Vec<Param>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub default: Option<Expr>,
    pub rest: bool,
}

#[derive(Debug, Clone)]
pub struct ClassDecl {
    pub name: String,
    pub base: Option<Vec<String>>,
    pub members: Vec<ClassMember>,
}

#[derive(Debug, Clone)]
pub enum ClassMember {
    Method(FunctionDecl),
    Var(Vec<(String, Option<Expr>)>),
    Const(Vec<(String, Option<Expr>)>),
    Event(EventDecl),
    Class(ClassDecl),
}

#[derive(Debug, Clone)]
pub struct EventDecl {
    pub name: String,
    pub selector: Option<Vec<TextPart>>,
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct SwitchCase {
    pub test: Option<Expr>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum TextPart {
    Text(String),
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Undefined,
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Unit(f64, String),
    Str(String),
    Symbol(String),
    Regex(String, String),
    This,
    Super,
    Ident(String),
    Array(Vec<Expr>),
    Map(Vec<(MapKey, Expr)>),
    Function(Vec<Param>, Vec<Stmt>),
    Arrow(Vec<Param>, Box<ArrowBody>),
    Jsx(JsxNode),
    Stringizer(Option<Box<Expr>>, String, Vec<TextPart>),
    Member(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    New(Box<Expr>, Vec<Expr>),
    Unary(&'static str, Box<Expr>),
    Postfix(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Assign(&'static str, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Comma(Box<Expr>, Box<Expr>),
    EventAttach(Box<Expr>, Box<EventDecl>),
    Delete(Box<Expr>),
    Let(String, Box<Expr>),
}

#[derive(Debug, Clone)]
pub enum ArrowBody {
    Expr(Expr),
    Block(Vec<Stmt>),
}

#[derive(Debug, Clone)]
pub enum MapKey {
    Ident(String),
    Str(String),
    Symbol(String),
    Int(i64),
}

#[derive(Debug, Clone)]
pub struct JsxNode {
    pub tag: String,
    pub type_suffix: Option<String>,
    pub name_binding: Option<String>,
    pub attrs: Vec<JsxAttr>,
    pub children: Vec<JsxChild>,
    pub self_closing: bool,
}

#[derive(Debug, Clone)]
pub enum JsxAttr {
    Id(Vec<TextPart>),
    Class(String),
    Ref(Expr),
    Splat(Expr),
    Named(String, Option<JsxAttrValue>),
}

#[derive(Debug, Clone)]
pub enum JsxAttrValue {
    Str(String),
    Expr(Expr),
    Int(i64),
    Float(f64),
    Unit(f64, String),
    Ident(String),
}

#[derive(Debug, Clone)]
pub enum JsxChild {
    Text(String),
    Expr(Expr),
    Element(JsxNode),
}
