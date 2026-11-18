//! JS ⇒ V8 bytecode: scopes, functions, statements, after V8's `BytecodeGenerator`. Expressions are in `expr.rs`.
//!
//! Variables live where V8 puts them: a function's own variables in registers (parameters are `a0` …, `this` the
//! receiver), variables inner functions capture in context slots (a context per scope level that has any), top-level
//! script `var`s / functions on the global object (`DeclareGlobals`), top-level script `let` / `const` / `class` in the
//! script context, names inside `with` or reachable by a sloppy direct eval looked up by name (`LdaLookupSlot`).
use std::collections::HashSet;

use oxc_allocator::Allocator;
use oxc_ast::ast::{Directive, BindingIdentifier, BindingPattern, BlockStatement, Class, Expression, MethodDefinitionKind, PropertyKey, ForStatementInit, FormalParameters, Function as AstFunction, ForInStatement, ForOfStatement, ForStatementLeft, Program, Statement, SwitchStatement, TryStatement, VariableDeclaration, VariableDeclarationKind};
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, SourceType, Span};
use j8framework::{CodeKind, CompareOp, Error, ImportEntry, ImportName, ModuleInfo, Source};

use crate::builder::{Builder, Label, RegisterList};
use crate::scope::{Decl, DeclKind, Decls, Hidden, Key, Resolver, THIS};
use j8bytecode::{Bytecode, Constant, Context, ContextVariable, CreateClosureFlags, FunctionKind, GlobalDeclaration, Module, Register, Runtime, ScopeInfo, ScopeType, SharedFunction, VariableMode};

/// finally continuation token: entered with an exception, rethrows on exit (V8's `kRethrowToken`)
const COMPLETION_THROW: i32 = j8bytecode::ContinuationToken::RETHROW;
/// finally continuation token: entered with a return value, keeps returning on exit
const COMPLETION_RETURN: i32 = 1;
/// finally continuation token: the n-th detoured break / continue is this number plus n
const COMPLETION_ROUTE: i32 = 2;
/// finally continuation token: normal fall-through (V8's `kFallthroughToken`)
const COMPLETION_NORMAL: i32 = j8bytecode::ContinuationToken::FALLTHROUGH;

/// Where a name is bound.
#[derive(Clone, Copy)]
enum Binding {
    Reg(Register),
    /// A slot of this level's context (the slot index includes the context's fixed slots)
    Context(u32),
    /// A module import cell (V8's `LdaModuleVariable`: imports are negative cell indices)
    Import(i32),
}

struct Bound<'a> {
    name: &'a str,
    binding: Binding,
    constant: bool,
    /// The temporal dead zone ends at this position (`u32::MAX`: always check)
    ready: Option<u32>,
    /// The own name of a named function expression (immutable)
    self_name: bool,
}

/// Binding name for `export default expression` / an anonymous default export
const DEFAULT_BINDING: &str = "*default*";

/// One scope level during code generation.
struct Level<'a> {
    /// Which function level it belongs to (index into `frames`; `usize::MAX`: the module scope)
    func: usize,
    names: Vec<Bound<'a>>,
    /// This level has a context: the register holding the outer context while it is current
    context: Option<Register>,
    /// The constant pool entry of its context's scope info
    scope: Option<usize>,
    /// Its context slots are accessed with context cells (V8's `has_context_cells`: the script context)
    cells: bool,
    /// Names not bound here may still be found at run time (a `with` object, `var`s a sloppy direct eval adds): they
    /// are looked up by name
    dynamic: bool,
    /// The register stack top before entering, restored on exit
    mark: i32,
    /// The level of a `switch`: cases may skip over declarations, so TDZ checks cannot be elided
    switch: bool,
}

/// Result of name resolution: where it is, whether it is constant, and whether reads / writes need a TDZ check (giving
/// the position where the declaration ends).
#[derive(Clone, Copy)]
pub struct Resolved<'a> {
    pub place: Place<'a>,
    pub constant: bool,
    /// `Some(ready)`: a check is needed if the use is before `ready`, or the binding is in a context / in a `switch`
    pub tdz: Option<(u32, bool)>,
}

impl Resolved<'_> {
    /// Whether a use at `pos` needs a TDZ check.
    pub fn needs_check(&self, pos: u32) -> bool {
        match self.tdz {
            Some((ready, always)) => always || pos < ready,
            None => false,
        }
    }
}

/// Where a name lives.
#[derive(Clone, Copy)]
pub enum Place<'a> {
    Reg(Register),
    Context { depth: u32, slot: u32, cells: bool },
    Global(&'a str),
    Import(i32),
    /// Looked up by name at run time (inside `with`, under a sloppy direct eval, from eval code)
    Lookup(&'a str),
}

/// Where a `break` / `continue` goes.
struct Jump<'a> {
    label: Option<&'a str>,
    /// Whether an unlabeled `break` can exit it (loops and `switch` can, labeled blocks cannot)
    breakable: bool,
    continuable: bool,
    breaks: Label,
    continues: Label,
    /// How many contexts this function had pushed on entering it: jumping out must pop the extra ones
    contexts: Vec<Register>,
}

/// A `break` / `continue` passing through a finally: the jump continues after the finally runs.
#[derive(Clone, Copy)]
struct Route<'a> {
    span: Span,
    label: Option<&'a str>,
    cont: bool,
}

/// A finally (V8's `TryFinallyBuilder` + `ControlScopeForTryFinally`): registers for the continuation token and
/// value, where it is entered, and the detoured jumps.
struct Finally<'a> {
    token: Register,
    value: Register,
    contexts: Vec<Register>,
    entry: Label,
    routes: Vec<Route<'a>>,
}

/// One level of the control stack: jump targets, a `try` region, a finally.
enum Control<'a> {
    Jump(Jump<'a>),
    /// Inside a `try` block (handled by the handler table: nothing to undo when jumping out)
    Handler,
    Finally(Finally<'a>),
    /// The iterator of a `for-of` (its record is in these three registers: iterator, next, done): must be closed before
    /// jumping out
    Iterator(Register),
    /// The iterator of a `for await` (iterator, next, done, sync): jumping out must call `return` and await it
    AsyncIterator(Register),
}

/// What kind of function, before V8's `FunctionKind` is picked (with generator / async from the flavor).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Normal,
    Arrow,
    Method,
    Getter,
    Setter,
    Constructor { derived: bool },
    /// Class field / static block initializers
    Initializer { statik: bool },
}

/// Function kind and flavor (generator, async).
#[derive(Clone, Copy, Default)]
pub struct Flavor {
    pub kind: Kind,
    pub generator: bool,
    pub asynchronous: bool,
    /// Strict on its own (`'use strict'` at the start of the body, inside a class body); strictness of an enclosing
    /// scope is inherited regardless
    pub strict: bool,
    /// A static class member
    pub statik: bool,
}

/// Flavor of a function node: generator / async from the node, kind given by the caller.
impl From<(&AstFunction<'_>, Kind)> for Flavor {
    fn from((f, kind): (&AstFunction<'_>, Kind)) -> Self {
        let strict = f.body.as_ref().is_some_and(|b| Self::use_strict(&b.directives));
        Self { kind, generator: f.generator, asynchronous: f.r#async, strict, statik: false }
    }
}

impl From<Kind> for Flavor {
    fn from(kind: Kind) -> Self {
        Self { kind, ..Self::default() }
    }
}

impl Flavor {
    /// Whether the directive prologue contains `'use strict'` (written exactly so, without escapes).
    pub fn use_strict(directives: &[Directive<'_>]) -> bool {
        directives.iter().any(|d| d.directive.as_str() == "use strict")
    }

    /// V8's `FunctionKind` for this flavor (`default`: a class without a `constructor`)
    pub fn v8_kind(self, default: bool) -> FunctionKind {
        let (g, a, s) = (self.generator, self.asynchronous, self.statik);
        match self.kind {
            Kind::Arrow if a => FunctionKind::AsyncArrowFunction,
            Kind::Arrow => FunctionKind::ArrowFunction,
            Kind::Getter if s => FunctionKind::StaticGetterFunction,
            Kind::Getter => FunctionKind::GetterFunction,
            Kind::Setter if s => FunctionKind::StaticSetterFunction,
            Kind::Setter => FunctionKind::SetterFunction,
            Kind::Constructor { derived: false } if default => FunctionKind::DefaultBaseConstructor,
            Kind::Constructor { derived: true } if default => FunctionKind::DefaultDerivedConstructor,
            Kind::Constructor { derived: false } => FunctionKind::BaseConstructor,
            Kind::Constructor { derived: true } => FunctionKind::DerivedConstructor,
            Kind::Initializer { statik: false } => FunctionKind::ClassMembersInitializerFunction,
            Kind::Initializer { statik: true } => FunctionKind::ClassStaticInitializerFunction,
            Kind::Method => match (g, a, s) {
                (true, true, false) => FunctionKind::AsyncConciseGeneratorMethod,
                (true, true, true) => FunctionKind::StaticAsyncConciseGeneratorMethod,
                (true, false, false) => FunctionKind::ConciseGeneratorMethod,
                (true, false, true) => FunctionKind::StaticConciseGeneratorMethod,
                (false, true, false) => FunctionKind::AsyncConciseMethod,
                (false, true, true) => FunctionKind::StaticAsyncConciseMethod,
                (false, false, false) => FunctionKind::ConciseMethod,
                (false, false, true) => FunctionKind::StaticConciseMethod,
            },
            Kind::Normal => match (g, a) {
                (true, true) => FunctionKind::AsyncGeneratorFunction,
                (true, false) => FunctionKind::GeneratorFunction,
                (false, true) => FunctionKind::AsyncFunction,
                (false, false) => FunctionKind::NormalFunction,
            },
        }
    }
}

/// Where a jump out lands: the jump target / finally of a control stack level, or leaving the function directly.
enum Exit {
    Jump(usize),
    Finally(usize),
    Function,
}

/// A function being compiled.
#[expect(clippy::struct_excessive_bools, reason = "state of a function being compiled: several unrelated flags")]
struct Frame<'a> {
    index: usize,
    b: Builder,
    name: Box<str>,
    flavor: Flavor,
    default_constructor: bool,
    length: u16,
    formals: u16,
    /// The function context's layout, when the function level has a context
    scope: Option<ScopeInfo>,
    /// The registers saving the outer contexts of the contexts this function has pushed so far (innermost last)
    contexts: Vec<Register>,
    /// How many loops enclose the current position (`JumpLoop`'s depth operand)
    loop_depth: u32,
    controls: Vec<Control<'a>>,
    /// The label for the next loop / `switch`
    label: Option<&'a str>,
    /// The script's completion value goes here (only the script itself has one)
    completion: Option<Register>,
    /// The generator object of a generator / async function (V8's generator register)
    generator: Option<Register>,
    /// Suspend points so far (`SuspendGenerator` ids)
    suspends: u32,
    /// Async generator: `yield` / `return` await the value first
    async_generator: bool,
    /// Strict mode
    strict: bool,
    /// annexB: block-level functions whose value must be written to the outer `var` when execution reaches the
    /// declaration (by start position of the function node)
    annexb: Vec<u32>,
    /// Index of the function-level scope (where `var`s live); global for a script
    var_level: Option<usize>,
    /// Eval code: names not bound in it are looked up by name
    eval: bool,
    /// Compiling the parameter prologue (an eval here cannot see the body's lexical declarations)
    prologue: bool,
    /// All parameters are simple names (no default values, destructuring, or rest)
    simple_params: bool,
}

impl Frame<'_> {
    /// The finished function
    fn finish(self) -> SharedFunction {
        SharedFunction {
            name: self.name,
            kind: self.flavor.v8_kind(self.default_constructor),
            strict: self.strict,
            length: self.length,
            formal_parameters: self.formals,
            scope: self.scope,
            bytecode: self.b.finish(),
        }
    }
}

/// A function body: statements, an arrow function's expression, class field initializers, a default constructor.
#[derive(Clone, Copy)]
pub enum Body<'a> {
    Statements(&'a [Statement<'a>]),
    Expression(&'a Expression<'a>),
    /// Instance (`statik` false) or static fields of a class: defined one by one on `this`
    Fields { class: &'a Class<'a>, statik: bool },
    /// A class without a `constructor`: a derived class forwards `super(...arguments)`
    DefaultConstructor { derived: bool },
}

pub struct Compiler<'a, 's> {
    src: &'s Source<'s>,
    /// The AST allocator: hidden binding names in class scopes (`#x` etc.) are allocated here too
    pub alloc: &'a Allocator,
    /// Referenced bindings (`new.target` / `arguments` are prepared only if used)
    used: HashSet<Key>,
    /// How many class bodies are being compiled (a class body is entirely strict code)
    class_depth: u32,
    captured: HashSet<Key>,
    module: Vec<SharedFunction>,
    frames: Vec<Frame<'a>>,
    levels: Vec<Level<'a>>,
    /// Optional chains being compiled: in each chain, `?.` jumps to the end of the chain on null / undefined
    pub chains: Vec<Label>,
    /// Functions containing a direct eval (by start position of the parameter list)
    eval_fns: HashSet<u32>,
    /// Completion value (V8's `Rewriter`): expression statements that assign it, and statements before which it is
    /// reset to `undefined` (by start position)
    completion_assign: HashSet<u32>,
    completion_reset: HashSet<u32>,
    /// While entering a `for` head's scope: the source text of its initializers
    for_init_text: Option<&'a str>,
}

/// V8's `Rewriter` (`Processor`): which statements of a script / eval code produce its completion value. Statements
/// are walked backwards; only the last value-producing statement assigns it, except inside breakable statements, where
/// a `break` / `continue` makes the one before it assign too.
#[derive(Default)]
struct Rewriter {
    is_set: bool,
    breakable: bool,
    assign: HashSet<u32>,
    reset: HashSet<u32>,
}

impl Rewriter {
    fn process<'a>(&mut self, stmts: impl DoubleEndedIterator<Item = &'a Statement<'a>>) {
        for s in stmts.rev() {
            if !(self.breakable || !self.is_set) {
                break;
            }
            self.visit(s);
        }
    }

    fn breakable(&mut self, f: impl FnOnce(&mut Self)) {
        let saved = core::mem::replace(&mut self.breakable, true);
        f(self);
        self.breakable = saved;
    }

    fn visit(&mut self, s: &Statement<'_>) {
        let start = s.span().start;
        match s {
            Statement::ExpressionStatement(_) => {
                if !self.is_set {
                    self.assign.insert(start);
                    self.is_set = true;
                }
            }
            Statement::BlockStatement(b) => self.process(b.body.iter()),
            Statement::IfStatement(i) => {
                let after = self.is_set;
                self.visit(&i.consequent);
                let in_then = self.is_set;
                self.is_set = after;
                if let Some(a) = &i.alternate {
                    self.visit(a);
                }
                if !(in_then && self.is_set) {
                    self.reset.insert(start);
                }
                self.is_set = true;
            }
            Statement::WhileStatement(_) | Statement::DoWhileStatement(_) | Statement::ForStatement(_) | Statement::ForInStatement(_) | Statement::ForOfStatement(_) => {
                let body = match s {
                    Statement::WhileStatement(w) => &w.body,
                    Statement::DoWhileStatement(w) => &w.body,
                    Statement::ForStatement(f) => &f.body,
                    Statement::ForInStatement(f) => &f.body,
                    Statement::ForOfStatement(f) => &f.body,
                    _ => unreachable!("a loop"),
                };
                self.breakable(|r| r.visit(body));
                self.reset.insert(start);
                self.is_set = true;
            }
            Statement::SwitchStatement(sw) => {
                self.breakable(|r| {
                    for c in sw.cases.iter().rev() {
                        r.process(c.consequent.iter());
                    }
                });
                self.reset.insert(start);
                self.is_set = true;
            }
            Statement::TryStatement(t) => {
                let after = self.is_set;
                self.process(t.block.body.iter());
                let in_try = self.is_set;
                if let Some(h) = &t.handler {
                    self.is_set = after;
                    self.process(h.body.body.iter());
                }
                if !(in_try && self.is_set) {
                    self.reset.insert(start);
                }
                self.is_set = true;
            }
            Statement::WithStatement(w) => {
                self.visit(&w.body);
                if !self.is_set {
                    self.reset.insert(start);
                }
                self.is_set = true;
            }
            Statement::LabeledStatement(l) => self.breakable(|r| r.visit(&l.body)),
            Statement::BreakStatement(_) | Statement::ContinueStatement(_) => self.is_set = false,
            _ => {}
        }
    }
}

/// JS ⇒ bytecode: every piece of JS (scripts, modules, `eval` / `Function` code) becomes a [`Module`] before it runs.
pub trait Compile: Sized {
    /// Compiles by kind: a script's top-level declarations go to the global object / script context; eval code's are
    /// declared at run time (`DeclareEvalVar` / `DeclareEvalFunction`) or stay within itself (strict). For a script the
    /// value of the last expression statement is the return value.
    fn compile(src: &Source<'_>, kind: CodeKind) -> Result<Self, Error>;

    /// Direct eval code: names it doesn't bind are looked up by name in the calling scope chain at run time.
    fn compile_direct(src: &Source<'_>, strict: bool) -> Result<Self, Error>;

    /// Compiles a module: gives the bytecode and the module info (requests, imports, exports, environment layout).
    fn compile_module(src: &Source<'_>) -> Result<(Self, ModuleInfo), Error>;
}

impl Compile for Module {
    fn compile(src: &Source<'_>, kind: CodeKind) -> Result<Self, Error> {
        Compiler::compile_in(src, kind, false)
    }

    fn compile_direct(src: &Source<'_>, strict: bool) -> Result<Self, Error> {
        Compiler::compile_in(src, if strict { CodeKind::StrictEval } else { CodeKind::Eval }, true)
    }

    fn compile_module(src: &Source<'_>) -> Result<(Self, ModuleInfo), Error> {
        Compiler::compile_module(src)
    }
}

impl Compiler<'_, '_> {
    fn compile_in(src: &Source<'_>, kind: CodeKind, direct: bool) -> Result<Module, Error> {
        let allocator = Allocator::default();
        // Script (not module); regular expression literals are validated during parsing
        let options = ParseOptions { parse_regular_expression: true, ..ParseOptions::default() };
        let ret = Parser::new(&allocator, src.text, SourceType::script()).with_options(options).parse();
        if let Some(d) = ret.diagnostics.first() {
            return Err(Error::Syntax(format!("{}: {d}", src.file)));
        }
        let program = &ret.program;
        // Early errors (duplicate declarations, illegal break / super / new.target, …): oxc's parser does not check
        // them, the semantic check is needed
        let checked = SemanticBuilder::new().with_check_syntax_error(true).build(program);
        if let Some(d) = checked.diagnostics.errors().next() {
            return Err(Error::Syntax(format!("{}: {d}", src.file)));
        }
        // Functions containing a direct eval: all their bindings go into contexts (an eval may read any of them)
        let scoping = checked.semantic.scoping();
        let eval_scopes: HashSet<oxc_syntax::scope::ScopeId> = scoping.scope_descendants_from_root().filter(|&id| scoping.scope_flags(id).contains_direct_eval()).collect();
        let strict = kind == CodeKind::StrictEval || Flavor::use_strict(&program.directives);
        let local = (kind != CodeKind::Script).then(|| Decls::eval_local(&program.body, strict));
        let (captured, used, eval_fns) = Resolver::script(&program.body, Decls::this_key(program.span), &allocator, local.as_ref(), strict, eval_scopes, None);
        let mut c = Compiler { src, alloc: &allocator, captured, used, class_depth: 0, module: Vec::new(), frames: Vec::new(), levels: Vec::new(), chains: Vec::new(), eval_fns, completion_assign: HashSet::new(), completion_reset: HashSet::new(), for_init_text: None };
        c.script(program, local.as_ref(), strict, direct)?;
        Ok(Module { functions: c.module })
    }
}

/// A top-level module name: a context slot (with whether it is a lexical declaration and whether it is const), or
/// an import binding.
#[derive(Clone, Copy)]
enum ModuleName {
    Slot { slot: u16, lexical: bool, constant: bool },
    Import(u16),
}

/// State while collecting top-level module names and module info.
struct Layout<'a> {
    names: Vec<(&'a str, ModuleName)>,
    info: ModuleInfo,
    slots: u16,
    /// (export name, local name): split into local exports and re-exports at the end
    local_exports: Vec<(&'a str, &'a str)>,
    /// Import binding name ⇒ import number
    imports: Vec<(&'a str, usize)>,
}

impl<'a> Layout<'a> {
    fn export_name(n: &oxc_ast::ast::ModuleExportName<'a>) -> &'a str {
        use oxc_ast::ast::ModuleExportName;
        match n {
            ModuleExportName::IdentifierName(i) => i.name.as_str(),
            ModuleExportName::IdentifierReference(i) => i.name.as_str(),
            ModuleExportName::StringLiteral(s) => s.value.as_str().unwrap_or_default(),
        }
    }

    fn request(&mut self, spec: &str) -> usize {
        match self.info.requests.iter().position(|r| **r == *spec) {
            Some(i) => i,
            None => {
                self.info.requests.push(spec.into());
                self.info.requests.len() - 1
            }
        }
    }

    fn declare(&mut self, name: &'a str, lexical: bool, constant: bool) {
        if self.names.iter().any(|(n, _)| *n == name) {
            return;
        }
        self.names.push((name, ModuleName::Slot { slot: self.slots, lexical, constant }));
        if lexical {
            self.info.lexical.push(self.slots);
        }
        self.slots += 1;
    }

    fn variables(&mut self, v: &'a VariableDeclaration<'a>) -> Vec<&'a str> {
        let mut ds = Decls::default();
        ds.declarators(v, DeclKind::Var);
        let lexical = v.kind != VariableDeclarationKind::Var;
        let names: Vec<&'a str> = ds.0.iter().map(|x| x.name).collect();
        for &n in &names {
            self.declare(n, lexical, v.kind == VariableDeclarationKind::Const);
        }
        names
    }

    fn declaration(&mut self, d: &'a oxc_ast::ast::Declaration<'a>) -> Vec<&'a str> {
        use oxc_ast::ast::Declaration;
        match d {
            Declaration::VariableDeclaration(v) => self.variables(v),
            Declaration::FunctionDeclaration(f) => f.id.iter().map(|id| {
                self.declare(id.name.as_str(), false, false);
                id.name.as_str()
            }).collect(),
            Declaration::ClassDeclaration(c) => c.id.iter().map(|id| {
                self.declare(id.name.as_str(), true, false);
                id.name.as_str()
            }).collect(),
            _ => Vec::new(),
        }
    }

    /// Exports: local bindings get slots; exports that are actually import bindings become indirect exports.
    fn finish(mut self) -> (Vec<(&'a str, ModuleName)>, ModuleInfo) {
        for (exported, local) in core::mem::take(&mut self.local_exports) {
            if let Some(&(_, idx)) = self.imports.iter().find(|(n, _)| *n == local) {
                let e = self.info.imports[idx].clone();
                self.info.indirect_exports.push((exported.into(), e.request, e.name));
            } else if let Some(&(_, ModuleName::Slot { slot, .. })) = self.names.iter().find(|(n, _)| *n == local) {
                self.info.local_exports.push((exported.into(), slot));
            }
        }
        self.info.slots = self.slots;
        (self.names, self.info)
    }
}

impl Compiler<'_, '_> {
    fn compile_module(src: &Source<'_>) -> Result<(Module, ModuleInfo), Error> {
        let allocator = Allocator::default();
        let options = ParseOptions { parse_regular_expression: true, ..ParseOptions::default() };
        let ret = Parser::new(&allocator, src.text, SourceType::mjs()).with_options(options).parse();
        if let Some(d) = ret.diagnostics.first() {
            return Err(Error::Syntax(format!("{}: {d}", src.file)));
        }
        let program = &ret.program;
        let checked = SemanticBuilder::new().with_check_syntax_error(true).build(program);
        if let Some(d) = checked.diagnostics.errors().next() {
            return Err(Error::Syntax(format!("{}: {d}", src.file)));
        }
        let scoping = checked.semantic.scoping();
        let eval_scopes: HashSet<oxc_syntax::scope::ScopeId> = scoping.scope_descendants_from_root().filter(|&id| scoping.scope_flags(id).contains_direct_eval()).collect();
        let (names, info) = Compiler::module_layout(program);
        let order: Vec<&str> = names.iter().map(|&(n, _)| n).collect();
        let (captured, used, eval_fns) = Resolver::script(&program.body, Decls::this_key(program.span), &allocator, None, true, eval_scopes, Some(&[order]));
        let mut c = Compiler { src, alloc: &allocator, captured, used, class_depth: 0, module: Vec::new(), frames: Vec::new(), levels: Vec::new(), chains: Vec::new(), eval_fns, completion_assign: HashSet::new(), completion_reset: HashSet::new(), for_init_text: None };
        let bound = names
            .iter()
            .map(|&(name, n)| match n {
                ModuleName::Slot { slot, lexical, constant } => Bound { name, binding: Binding::Context(Context::MIN_SLOTS + u32::from(slot)), constant, ready: lexical.then_some(u32::MAX), self_name: false },
                ModuleName::Import(i) => Bound { name, binding: Binding::Import(-1 - i32::from(i)), constant: true, ready: None, self_name: false },
            })
            .collect();
        c.levels.push(Level { func: usize::MAX, names: bound, context: Some(Register::current_context()), scope: None, dynamic: false, mark: 0, switch: false, cells: false });
        c.module_body(program)?;
        Ok((Module { functions: c.module }, info))
    }

    /// Top-level module names and module info: slot 0 is `this` (undefined), then in declaration order.
    fn module_layout<'a>(program: &'a Program<'a>) -> (Vec<(&'a str, ModuleName)>, ModuleInfo) {
        use oxc_ast::ast::{ExportDefaultDeclarationKind, ImportDeclarationSpecifier};
        let mut l = Layout { names: alloc::vec![(THIS, ModuleName::Slot { slot: 0, lexical: false, constant: true })], info: ModuleInfo::default(), slots: 1, local_exports: Vec::new(), imports: Vec::new() };
        for s in &program.body {
            match s {
                Statement::ImportDeclaration(i) => {
                    let req = l.request(i.source.value.as_str().unwrap_or_default());
                    for sp in i.specifiers.iter().flatten() {
                        let (local, name) = match sp {
                            ImportDeclarationSpecifier::ImportSpecifier(x) => (x.local.name.as_str(), ImportName::Named(Layout::export_name(&x.imported).into())),
                            ImportDeclarationSpecifier::ImportDefaultSpecifier(x) => (x.local.name.as_str(), ImportName::Named("default".into())),
                            ImportDeclarationSpecifier::ImportNamespaceSpecifier(x) => (x.local.name.as_str(), ImportName::Namespace),
                        };
                        let idx = u16::try_from(l.info.imports.len()).unwrap_or(u16::MAX);
                        l.info.imports.push(ImportEntry { request: req, name });
                        l.names.push((local, ModuleName::Import(idx)));
                        l.imports.push((local, usize::from(idx)));
                    }
                }
                Statement::ExportDeclaration(e) => {
                    for n in l.declaration(&e.declaration) {
                        l.local_exports.push((n, n));
                    }
                }
                Statement::ExportFromDeclaration(e) => {
                    let req = l.request(e.source.value.as_str().unwrap_or_default());
                    for sp in &e.specifiers {
                        l.info.indirect_exports.push((Layout::export_name(&sp.exported).into(), req, ImportName::Named(Layout::export_name(&sp.local).into())));
                    }
                }
                Statement::ExportNamedDeclaration(e) => {
                    for sp in &e.specifiers {
                        l.local_exports.push((Layout::export_name(&sp.exported), Layout::export_name(&sp.local)));
                    }
                }
                Statement::ExportDefaultDeclaration(e) => {
                    let local = match &e.declaration {
                        ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                            let n = f.id.as_ref().map_or(DEFAULT_BINDING, |id| id.name.as_str());
                            l.declare(n, false, false);
                            n
                        }
                        ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                            let n = c.id.as_ref().map_or(DEFAULT_BINDING, |id| id.name.as_str());
                            l.declare(n, true, false);
                            n
                        }
                        _ => {
                            l.declare(DEFAULT_BINDING, true, false);
                            DEFAULT_BINDING
                        }
                    };
                    l.local_exports.push(("default", local));
                }
                Statement::ExportAllDeclaration(e) => {
                    let req = l.request(e.source.value.as_str().unwrap_or_default());
                    match &e.exported {
                        Some(n) => l.info.indirect_exports.push((Layout::export_name(n).into(), req, ImportName::Namespace)),
                        None => l.info.star_exports.push(req),
                    }
                }
                Statement::VariableDeclaration(v) => _ = l.variables(v),
                Statement::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        l.declare(id.name.as_str(), false, false);
                    }
                }
                Statement::ClassDeclaration(c) => {
                    if let Some(id) = &c.id {
                        l.declare(id.name.as_str(), true, false);
                    }
                }
                _ => {}
            }
        }
        // `var`s in blocks are also at the module top level
        let mut vars = Decls::default();
        vars.vars(&program.body);
        for d in &vars.0 {
            l.declare(d.name, false, false);
        }
        l.finish()
    }
}

impl<'a> Compiler<'a, '_> {
    // ---- Basics ------------------------------------------------------------------------

    /// The file name being compiled (base for relative paths in dynamic import).
    pub fn file(&self) -> &'a str {
        self.alloc.alloc_str(self.src.file)
    }

    pub fn unsupported(&self, span: Span, what: &str) -> Error {
        self.src.unsupported(span.start, what)
    }

    /// annexB: when execution reaches a block-level function declaration, writes the function value from the block to
    /// the outer `var` (a function-level binding or a global).
    fn annexb_copy(&mut self, f: &'a oxc_ast::ast::Function<'a>) {
        let Some(id) = &f.id else { return };
        if !self.frame().annexb.contains(&f.span.start) {
            return;
        }
        let name = id.name.as_str();
        self.load(name);
        let (var_level, eval) = (self.frame().var_level, self.frame().eval);
        match var_level {
            Some(level) => {
                let mut depth = 0;
                for (i, l) in self.levels.iter().enumerate().rev() {
                    if i <= level
                        && let Some(n) = l.names.iter().find(|n| n.name == name)
                    {
                        match n.binding {
                            Binding::Reg(r) => self.b().star(r),
                            Binding::Context(slot) => self.b().store_context_slot(Register::current_context(), slot, depth),
                            Binding::Import(_) => self.throw_const_assign(),
                        }
                        return;
                    }
                    if l.context.is_some() {
                        depth += 1;
                    }
                }
            }
            None if eval => {
                let flags = j8bytecode::StoreLookupSlotFlags::LOOKUP_HOISTING;
                self.b().store_lookup_slot(name, flags);
            }
            None => self.b().store_global(name, false),
        }
    }

    /// `CallRuntime [ThrowConstAssignError]`
    pub fn throw_const_assign(&mut self) {
        self.b().call_runtime(Runtime::ThrowConstAssignError, RegisterList::EMPTY);
    }

    /// Whether the name resolves to the own name of a named function expression.
    fn is_self_name(&self, name: &str) -> bool {
        self.levels.iter().rev().find_map(|l| l.names.iter().find(|n| n.name == name)).is_some_and(|n| n.self_name)
    }

    /// Whether the code here is strict: the function itself is strict, or it is inside a class body (`extends` and
    /// computed keys are evaluated in the outer function, also as strict code).
    pub fn strict_here(&self) -> bool {
        self.strict() || self.class_depth > 0
    }

    /// Whether the current function is in strict mode.
    pub fn strict(&self) -> bool {
        self.frames.last().is_some_and(|f| f.strict)
    }

    fn frame(&mut self) -> &mut Frame<'a> {
        self.frames.last_mut().expect("inside a function")
    }

    /// The current function's bytecode builder
    pub fn b(&mut self) -> &mut Builder {
        &mut self.frame().b
    }

    pub fn constant(&mut self, c: Constant) -> usize {
        self.b().constant(c)
    }

    pub fn name(&mut self, s: &str) -> usize {
        self.b().name(s)
    }

    /// `n` consecutive temporary registers, [`release`](Self::release)d in stack order.
    pub fn temps(&mut self, n: u32) -> Register {
        self.b().new_register_list(n).first
    }

    pub fn temp(&mut self) -> Register {
        self.temps(1)
    }

    pub fn release(&mut self, r: Register) {
        self.b().release_to(r.0);
    }

    /// `CreateClosure` of function `index` of the module (V8's flags: fast new closure inside a function scope)
    pub fn closure(&mut self, index: u32) {
        let k = self.constant(Constant::Function(index));
        let in_function = self.frames.len() > 1 || self.levels.first().is_some_and(|l| l.func == usize::MAX);
        self.b().create_closure(k, CreateClosureFlags::encode(false, in_function));
    }

    // ---- Scopes ------------------------------------------------------------------------

    /// Enters a scope level: allocates bindings (captured ones go into a new context of `scope_type`), copies in the
    /// captured `this` / parameters, creates closures for function declarations. A function level's context is a
    /// function context, recorded as the function's scope.
    fn enter(&mut self, decls: &Decls<'a>, scope_type: ScopeType) -> Result<(), Error> {
        let func = self.frames.len() - 1;
        let mark = self.b().mark();
        let mut slots = 0u32;
        let mut names = Vec::with_capacity(decls.0.len());
        let mut variables = Vec::new();
        for d in &decls.0 {
            // Pseudo-bindings nobody reads take no register
            if matches!(d.kind, DeclKind::NewTarget | DeclKind::Arguments) && !self.used.contains(&d.key) {
                continue;
            }
            let binding = if self.captured.contains(&d.key) {
                slots += 1;
                let mode = match d.kind {
                    DeclKind::Let => VariableMode::Let,
                    DeclKind::Const => VariableMode::Const,
                    _ => VariableMode::Var,
                };
                variables.push(ContextVariable { name: d.name.into(), mode });
                Binding::Context(Context::MIN_SLOTS + slots - 1)
            } else {
                match d.kind {
                    DeclKind::This => Binding::Reg(Register::receiver()),
                    DeclKind::SelfName => Binding::Reg(Register::function_closure()),
                    DeclKind::Param(i) => Binding::Reg(Register::parameter(i32::from(i) + 1)),
                    _ => Binding::Reg(self.temp()),
                }
            };
            names.push(Bound { name: d.name, binding, constant: matches!(d.kind, DeclKind::Const), ready: d.ready, self_name: matches!(d.kind, DeclKind::SelfName) });
        }
        let mut scope = None;
        let context = if slots > 0 {
            let info = ScopeInfo { scope_type: Some(scope_type), variables, has_extension: false };
            let k = self.constant(Constant::Scope(info.clone()));
            scope = Some(k);
            match scope_type {
                ScopeType::FunctionScope => {
                    self.frame().scope = Some(info);
                    self.b().create_function_context(k, slots);
                }
                _ => self.b().create_block_context(k),
            }
            let saved = self.temp();
            self.b().push_context(saved);
            self.frame().contexts.push(saved);
            Some(saved)
        } else {
            None
        };
        let bindings: Vec<Binding> = names.iter().map(|n| n.binding).collect();
        self.levels.push(Level { func, names, context, scope, dynamic: false, mark, switch: false, cells: false });
        let decls: Vec<&Decl<'a>> = decls.0.iter().filter(|d| !(matches!(d.kind, DeclKind::NewTarget | DeclKind::Arguments) && !self.used.contains(&d.key))).collect();
        // Temporal dead zone: slots are first filled with the hole (not for a `for` head's register variable its own
        // initializer doesn't mention: nothing can read it before it is initialized)
        for (d, &b) in decls.iter().zip(&bindings) {
            let skip = self.for_init_text.is_some_and(|t| !t.contains(d.name)) && matches!(b, Binding::Reg(_));
            if d.ready.is_some() && !skip {
                self.b().load_the_hole();
                self.bind(b);
            }
        }
        for (d, b) in decls.iter().zip(bindings) {
            let fixed = match d.kind {
                DeclKind::This => Some(Register::receiver()),
                DeclKind::SelfName => Some(Register::function_closure()),
                DeclKind::Param(i) => Some(Register::parameter(i32::from(i) + 1)),
                DeclKind::Function(f) => {
                    let n = self.function_declaration(f)?;
                    self.closure(n);
                    self.bind(b);
                    None
                }
                DeclKind::NewTarget if self.used.contains(&d.key) => {
                    // The interpreter puts new.target into this register on entry (V8's
                    // `incoming_new_target_or_generator_register`)
                    let r = self.temp();
                    self.b().new_target_or_generator = Some(r.0);
                    self.b().ldar(r);
                    self.bind(b);
                    None
                }
                DeclKind::Arguments if self.used.contains(&d.key) => {
                    // Sloppy functions with simple parameters get a mapped arguments object (aliasing the parameters)
                    let mapped = self.frame().simple_params && !self.frame().strict;
                    self.b().create_arguments(mapped);
                    self.bind(b);
                    None
                }
                DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::NewTarget | DeclKind::Arguments => None,
            };
            if let (Some(r), Binding::Context(_)) = (fixed, b) {
                self.b().ldar(r);
                self.bind(b);
            }
        }
        Ok(())
    }

    /// Stores acc into the binding just allocated at this level.
    fn bind(&mut self, b: Binding) {
        match b {
            Binding::Reg(r) => self.b().star(r),
            Binding::Context(slot) => self.b().store_context_slot(Register::current_context(), slot, 0),
            Binding::Import(_) => self.throw_const_assign(),
        }
    }

    /// Leaves a block scope level.
    fn leave(&mut self) {
        let l = self.levels.pop().expect("a scope to leave");
        if let Some(saved) = l.context {
            self.b().pop_context(saved);
            self.frame().contexts.pop();
        }
        self.b().release_to(l.mark);
    }

    fn scoped(&mut self, decls: &Decls<'a>, f: impl FnOnce(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        self.enter(decls, ScopeType::BlockScope)?;
        f(self)?;
        self.leave();
        Ok(())
    }

    /// Name resolution: innermost first, counting the contexts passed; a dynamic level passed makes it a lookup by name,
    /// so do names eval code doesn't bind; global otherwise.
    pub fn resolve(&self, name: &'a str) -> Resolved<'a> {
        let cur = self.frames.len() - 1;
        let mut depth = 0;
        for l in self.levels.iter().rev() {
            if let Some(n) = l.names.iter().find(|n| n.name == name) {
                let (place, in_context) = match n.binding {
                    Binding::Reg(r) => {
                        assert_eq!(l.func, cur, "an outer function's binding `{name}` used from inside must be captured");
                        (Place::Reg(r), false)
                    }
                    Binding::Context(slot) => (Place::Context { depth, slot, cells: l.cells }, true),
                    Binding::Import(i) => (Place::Import(i), false),
                };
                // Inside the declaring function the position decides; from an inner function (which may run any time)
                // or a `switch` (whose cases may skip the declaration) always check (V8's hole check elimination)
                return Resolved { place, constant: n.constant, tdz: n.ready.map(|r| (r, (in_context && l.func != cur) || l.switch)) };
            }
            if l.dynamic {
                return Resolved { place: Place::Lookup(name), constant: false, tdz: None };
            }
            if l.context.is_some() {
                depth += 1;
            }
        }
        if self.frames.first().is_some_and(|f| f.eval) {
            return Resolved { place: Place::Lookup(name), constant: false, tdz: None };
        }
        Resolved { place: Place::Global(name), constant: false, tdz: None }
    }

    /// If the name is in this function's registers and needs no TDZ check at `pos`, gives that register. Only this
    /// function's own code can change it (bindings captured by inner functions are in contexts, not registers).
    pub fn register_of(&self, name: &'a str, pos: u32) -> Option<Register> {
        let r = self.resolve(name);
        match r.place {
            Place::Reg(reg) if !r.needs_check(pos) => Some(reg),
            _ => None,
        }
    }

    pub fn lookup(&self, name: &'a str) -> (Place<'a>, bool) {
        let r = self.resolve(name);
        (r.place, r.constant)
    }

    /// Reads a name into acc (use position unknown, so the TDZ is checked).
    pub fn load(&mut self, name: &'a str) {
        self.load_at(name, 0);
    }

    /// Reads a name for `typeof`: an unresolvable name gives `undefined` instead of throwing
    pub fn load_typeof(&mut self, name: &'a str) {
        match self.resolve(name).place {
            Place::Global(g) if !matches!(g, "undefined" | "NaN" | "Infinity") => self.b().load_global(g, true),
            Place::Lookup(g) => self.b().load_lookup_slot(g, true),
            _ => self.load_at(name, 0),
        }
    }

    /// Reads a name into acc at source position `pos`. `undefined` / `NaN` / `Infinity` are constants unless shadowed.
    pub fn load_at(&mut self, name: &'a str, pos: u32) {
        let r = self.resolve(name);
        match r.place {
            Place::Reg(reg) => self.b().ldar(reg),
            Place::Context { depth, slot, cells } => {
                let immutable = r.constant && !r.needs_check(pos);
                self.b().load_context_slot(Register::current_context(), slot, depth, immutable, cells);
            }
            Place::Global("undefined") => self.b().load_undefined(),
            Place::Global("NaN") => self.b().load_number(f64::NAN),
            Place::Global("Infinity") => self.b().load_number(f64::INFINITY),
            Place::Global(g) => self.b().load_global(g, false),
            Place::Import(cell) => _ = self.b().raw(Bytecode::LdaModuleVariable, &[cell.into(), 0]),
            Place::Lookup(g) => self.b().load_lookup_slot(g, false),
        }
        if r.needs_check(pos) {
            if name == THIS {
                self.b().plain(Bytecode::ThrowSuperNotCalledIfHole);
            } else {
                self.b().throw_reference_error_if_hole(name);
            }
        }
    }

    /// Stores acc into a name; `init` is the initialization in a declaration (the only time `const` allows it, with no
    /// TDZ check).
    pub fn store(&mut self, name: &'a str, init: bool) {
        self.store_at(name, init, 0);
    }

    /// Same as [`store`](Self::store); `pos` is the source position of the assignment: no TDZ check after the
    /// declaration. acc is kept.
    pub fn store_at(&mut self, name: &'a str, init: bool, pos: u32) {
        if !init && self.is_self_name(name) {
            if self.strict_here() {
                self.throw_const_assign();
            }
            return;
        }
        let r = self.resolve(name);
        let check = !init && r.needs_check(pos);
        if check || (r.constant && !init) {
            // V8's `BuildVariableAssignment`: the hole check reads the variable, so the value waits in a register
            let value = self.temp();
            self.b().star(value);
            if check {
                self.load_at(name, 0);
            }
            if r.constant && !init {
                self.throw_const_assign();
            }
            self.b().ldar(value);
            self.release(value);
            if r.constant && !init {
                return;
            }
        }
        match r.place {
            Place::Reg(reg) => self.b().star(reg),
            Place::Context { depth, slot, cells } => self.b().store_context_slot_cells(Register::current_context(), slot, depth, cells),
            Place::Global(g) => {
                let strict = self.strict_here();
                self.b().store_global(g, strict);
            }
            Place::Import(_) => self.throw_const_assign(),
            Place::Lookup(g) => {
                let flags = if self.strict_here() { j8bytecode::StoreLookupSlotFlags::STRICT } else { 0 };
                self.b().store_lookup_slot(g, flags);
            }
        }
    }

    // ---- Functions ---------------------------------------------------------------------

    /// Compiles a function, returning its index in the module.
    #[expect(clippy::too_many_lines, reason = "function prologue follows V8's GenerateBytecodeBody step by step")]
    pub fn function(&mut self, name: &str, flavor: Flavor, this: Option<Key>, self_name: Option<&'a BindingIdentifier<'a>>, params: Option<&'a FormalParameters<'a>>, body: Body<'a>) -> Result<u32, Error> {
        let count = params.map_or(0, |p| p.items.len());
        let n = u16::try_from(count).map_err(|_| self.unsupported(params.map_or(Span::default(), |p| p.span), "more than 65535 parameters"))?;
        let index = self.module.len();
        // A placeholder until the function is done (inner functions get the next indices)
        self.module.push(SharedFunction { name: "".into(), kind: FunctionKind::NormalFunction, strict: false, length: 0, formal_parameters: 0, scope: None, bytecode: j8bytecode::BytecodeArray::default() });
        // length: the number of parameters before the first one with a default value (the rest parameter does not
        // count)
        let length = u16::try_from(params.map_or(0, |p| p.items.iter().take_while(|i| i.initializer.is_none()).count())).unwrap_or(u16::MAX);
        // Functions in a class (methods, constructors, field initializers) are all strict
        let strict = flavor.strict || self.class_depth > 0 || self.frames.last().is_some_and(|f| f.strict);
        let default_constructor = matches!(body, Body::DefaultConstructor { .. });
        let simple_params = params.is_some_and(|p| p.rest.is_none() && p.items.iter().all(|i| i.initializer.is_none() && matches!(i.pattern, BindingPattern::BindingIdentifier(_))));
        self.frames.push(Frame {
            index,
            b: Builder::new(n + 1, 0),
            name: name.into(),
            flavor,
            default_constructor,
            length,
            formals: n,
            scope: None,
            contexts: Vec::new(),
            loop_depth: 0,
            controls: Vec::new(),
            label: None,
            completion: None,
            generator: None,
            suspends: 0,
            async_generator: flavor.generator && flavor.asynchronous,
            strict,
            annexb: Vec::new(),
            var_level: None,
            eval: false,
            prologue: false,
            simple_params,
        });
        if flavor.generator || flavor.asynchronous {
            // The interpreter puts the generator object into this register on entry
            let g = self.temp();
            self.b().new_target_or_generator = Some(g.0);
            self.frame().generator = Some(g);
        }
        let stmts = match body {
            Body::Statements(s) => s,
            _ => &[],
        };
        let sloppy_eval = params.is_some_and(|p| !strict && self.eval_fns.contains(&p.span.start));
        // A mapped arguments object aliases the parameters: they live in the context (V8 forces them there)
        let decls = Decls::function(this, self_name, params, stmts, !strict, None);
        if simple_params && !strict && decls.0.iter().any(|d| matches!(d.kind, DeclKind::Arguments) && self.used.contains(&d.key)) {
            for d in &decls.0 {
                if let DeclKind::Param(_) = d.kind {
                    self.captured.insert(d.key);
                }
            }
        }
        self.enter(&decls, ScopeType::FunctionScope)?;
        if sloppy_eval && let Some(l) = self.levels.last_mut() {
            // `var`s a sloppy direct eval declares land in this function's context extension: names not bound here
            // are looked up by name
            l.dynamic = true;
            if let Some(s) = self.frames.last_mut().and_then(|f| f.scope.as_mut()) {
                s.has_extension = true;
            }
        }
        if !strict {
            self.frame().var_level = Some(self.levels.len() - 1);
            self.frame().annexb = Decls::annexb_functions(stmts, params).iter().map(|f| f.span.start).collect();
        }
        if flavor.kind == (Kind::Constructor { derived: true })
            && let Some(t) = self.levels.last_mut().and_then(|l| l.names.iter_mut().find(|n| n.name == THIS))
        {
            // In a derived class constructor `this` is the hole before `super()`
            t.ready = Some(u32::MAX);
        }
        if let Some(p) = params {
            self.frame().prologue = true;
            self.prologue(p)?;
            self.frame().prologue = false;
        }
        // Generator: suspend only after parameters and declarations are set up (a call runs up to here, so parameter
        // errors throw synchronously); the first next resumes from here
        if flavor.generator && !flavor.asynchronous {
            self.b().load_undefined();
            self.initial_yield()?;
        }
        match body {
            Body::Statements(s) => self.statements(s)?,
            Body::Expression(e) => {
                self.expr(e)?;
                self.ret();
            }
            Body::Fields { class, statik } => self.fields(class, statik)?,
            Body::DefaultConstructor { derived } => {
                if derived {
                    self.super_call_forward()?;
                }
            }
        }
        self.b().load_undefined();
        self.ret();
        self.levels.pop();
        let f = self.frames.pop().expect("the function's frame");
        let at = f.index;
        self.module[at] = f.finish();
        Ok(u32::try_from(index).expect("fewer than 2^32 functions"))
    }

    /// Parameter prologue: default values, destructured parameters, rest parameter.
    fn prologue(&mut self, params: &'a FormalParameters<'a>) -> Result<(), Error> {
        for (i, p) in (0u16..).zip(&params.items) {
            let reg = Register::parameter(i32::from(i) + 1);
            if let Some(init) = &p.initializer {
                self.b().ldar(reg);
                let mut skip = Label::default();
                self.b().jump(Bytecode::JumpIfNotUndefined, &mut skip);
                match &p.pattern {
                    BindingPattern::BindingIdentifier(id) => self.expr_named(init, id.name.as_str())?,
                    _ => self.expr(init)?,
                }
                self.b().star(reg);
                self.b().bind(&mut skip);
            }
            match &p.pattern {
                BindingPattern::BindingIdentifier(id) => {
                    // Captured parameters were copied into the context on frame entry; after the default value is
                    // computed they must be copied again
                    let name = id.name.as_str();
                    if p.initializer.is_some() && !matches!(self.lookup(name).0, Place::Reg(r) if r == reg) {
                        self.b().ldar(reg);
                        self.store(name, true);
                    }
                }
                pat => self.bind_pattern(pat, reg, true)?,
            }
        }
        if let Some(r) = &params.rest {
            self.b().create_rest_parameter();
            let t = self.temp();
            self.b().star(t);
            self.bind_pattern(&r.rest.argument, t, true)?;
            self.release(t);
        }
        Ok(())
    }

    /// Binds the value in register `value` to names according to the pattern (`init`: initialization in a declaration).
    pub fn bind_pattern(&mut self, p: &'a BindingPattern<'a>, value: Register, init: bool) -> Result<(), Error> {
        match p {
            BindingPattern::BindingIdentifier(id) => {
                self.b().ldar(value);
                self.store(id.name.as_str(), init);
            }
            BindingPattern::AssignmentPattern(a) => {
                let name = match &a.left {
                    BindingPattern::BindingIdentifier(id) => Some(id.name.as_str()),
                    _ => None,
                };
                let t = self.with_default(value, &a.right, name)?;
                self.bind_pattern(&a.left, t, init)?;
                self.release(t);
            }
            BindingPattern::ObjectPattern(o) => {
                let n = u32::try_from(o.properties.len()).map_err(|_| self.unsupported(o.span, "too many properties"))?;
                if o.properties.is_empty() {
                    self.require_object(value);
                }
                // With a rest element the keys go after a copy of the source, ready for
                // `CopyDataPropertiesWithExcludedPropertiesOnStack(source, ...keys)`
                let list = o.rest.as_ref().map(|_| {
                    let l = self.b().new_register_list(n + 1);
                    self.b().mov(value, l.first);
                    l
                });
                for (i, b) in (0u32..).zip(&o.properties) {
                    let t = self.temp();
                    match (self.static_name(&b.key, b.computed), list) {
                        (Some(name), None) => self.b().get_named(value, name),
                        (_, l) => {
                            let k = l.map_or_else(|| self.temp(), |l| l.at(i + 1));
                            self.property_key_into(&b.key, b.computed, k)?;
                            self.b().ldar(k);
                            self.b().get_keyed(value);
                            if l.is_none() {
                                self.release(k);
                            }
                        }
                    }
                    self.b().star(t);
                    self.bind_pattern(&b.value, t, init)?;
                    self.release(t);
                }
                if let (Some(r), Some(l)) = (&o.rest, list) {
                    self.b().call_runtime(Runtime::InlineCopyDataPropertiesWithExcludedPropertiesOnStack, l);
                    let t = self.temp();
                    self.b().star(t);
                    self.bind_pattern(&r.argument, t, init)?;
                    self.release(t);
                    self.release(l.first);
                }
            }
            BindingPattern::ArrayPattern(a) => {
                let iter = self.iterate(value);
                // If a default value throws, the iterator must be closed first
                self.closing_on_throw(iter, |c| {
                    for e in &a.elements {
                        c.iterator_next(iter);
                        let Some(e) = e else { continue };
                        let t = c.temp();
                        c.b().star(t);
                        c.bind_pattern(e, t, init)?;
                        c.release(t);
                    }
                    match &a.rest {
                        Some(r) => {
                            c.iterator_rest(iter);
                            let t = c.temp();
                            c.b().star(t);
                            c.bind_pattern(&r.argument, t, init)?;
                            c.release(t);
                        }
                        None => c.iterator_close(iter),
                    }
                    Ok(())
                })?;
                self.release(iter);
            }
        }
        Ok(())
    }

    /// `body` in a `try` region whose handler runs `catch` with the exception in acc (V8's `TryCatchBuilder`: the
    /// context is saved in a register first; the handler table restores it)
    pub fn try_catch(&mut self, prediction: j8bytecode::CatchPrediction, body: impl FnOnce(&mut Self) -> Result<(), Error>, catch: impl FnOnce(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        let ctx = self.temp();
        self.b().mov(Register::current_context(), ctx);
        let h = self.b().new_handler(ctx, prediction);
        self.b().try_start(h);
        self.frame().controls.push(Control::Handler);
        body(self)?;
        self.frame().controls.pop();
        self.b().try_end(h);
        let mut done = Label::default();
        self.b().jump(Bytecode::Jump, &mut done);
        self.b().handler_target(h);
        catch(self)?;
        self.b().bind(&mut done);
        self.release(ctx);
        Ok(())
    }

    /// Runs `f`; if it throws, closes iterator `iter` first (calling `return` only if not done, ignoring what it does)
    /// and then rethrows.
    pub fn closing_on_throw(&mut self, iter: Register, f: impl FnOnce(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        self.try_catch(j8bytecode::CatchPrediction::Uncaught, f, |c| {
            let e = c.temp();
            c.b().star(e);
            c.iterator_close_quiet(iter);
            c.b().ldar(e);
            c.b().plain(Bytecode::ReThrow);
            c.release(e);
            Ok(())
        })
    }

    /// Gets the iterator of register `value`: the iterator record takes three consecutive registers (iterator, next,
    /// done; the caller frees them)
    pub fn iterate(&mut self, value: Register) -> Register {
        let rec = self.temps(3);
        self.b().get_iterator(value);
        self.b().star(rec);
        self.b().get_named(rec, "next");
        self.b().star(Register(rec.0 + 1));
        self.b().load_bool(false);
        self.b().star(Register(rec.0 + 2));
        rec
    }

    /// Calls the record's `next`, checks the result is an object, leaves it in acc
    fn step_result(&mut self, rec: Register) {
        let args = RegisterList { first: rec, count: 1 };
        self.b().call_property(Register(rec.0 + 1), args);
        let mut ok = Label::default();
        self.b().jump(Bytecode::JumpIfJSReceiver, &mut ok);
        let r = self.temp();
        self.b().star(r);
        self.b().call_runtime(Runtime::ThrowIteratorResultNotAnObject, RegisterList { first: r, count: 1 });
        self.release(r);
        self.b().bind(&mut ok);
    }

    /// The next value of the iterator record into acc (`undefined` once done); an error while stepping marks it done
    pub fn iterator_next(&mut self, rec: Register) {
        let done = Register(rec.0 + 2);
        let mut finished = Label::default();
        let mut end = Label::default();
        self.b().ldar(done);
        self.b().jump(Bytecode::JumpIfTrue, &mut finished);
        self.b().load_bool(true);
        self.b().star(done);
        self.step_result(rec);
        let r = self.temp();
        self.b().star(r);
        self.b().get_named(r, "done");
        self.b().jump(Bytecode::JumpIfToBooleanTrue, &mut finished);
        self.b().get_named(r, "value");
        self.b().star(r);
        self.b().load_bool(false);
        self.b().star(done);
        self.b().ldar(r);
        self.release(r);
        self.b().jump(Bytecode::Jump, &mut end);
        self.b().bind(&mut finished);
        self.b().load_undefined();
        self.b().bind(&mut end);
    }

    /// The rest of the iterator record's values as a new array in acc
    pub fn iterator_rest(&mut self, rec: Register) {
        let done = Register(rec.0 + 2);
        let arr = self.temp();
        let index = self.temp();
        let r = self.temp();
        self.b().create_empty_array_literal();
        self.b().star(arr);
        self.b().load_smi(0);
        self.b().star(index);
        let mut end = Label::default();
        self.b().ldar(done);
        self.b().jump(Bytecode::JumpIfTrue, &mut end);
        self.b().load_bool(true);
        self.b().star(done);
        let header = self.b().loop_header();
        self.frame().loop_depth += 1;
        self.step_result(rec);
        self.b().star(r);
        self.b().get_named(r, "done");
        self.b().jump(Bytecode::JumpIfToBooleanTrue, &mut end);
        self.b().get_named(r, "value");
        self.b().store_in_array_literal(arr, index);
        self.b().ldar(index);
        self.b().unary(Bytecode::Inc);
        self.b().star(index);
        let depth = self.frame().loop_depth - 1;
        self.b().jump_loop(header, depth);
        self.frame().loop_depth -= 1;
        self.b().bind(&mut end);
        self.b().ldar(arr);
        self.release(arr);
    }

    /// Closes the iterator record unless done (V8's `BuildIteratorClose`): calls `return` if there is one and checks its
    /// result is an object. acc is clobbered.
    pub fn iterator_close(&mut self, rec: Register) {
        let mut end = Label::default();
        self.b().ldar(Register(rec.0 + 2));
        self.b().jump(Bytecode::JumpIfTrue, &mut end);
        self.b().get_named(rec, "return");
        self.b().jump(Bytecode::JumpIfUndefinedOrNull, &mut end);
        let m = self.temp();
        self.b().star(m);
        self.b().call_property(m, RegisterList { first: rec, count: 1 });
        self.b().jump(Bytecode::JumpIfJSReceiver, &mut end);
        self.b().star(m);
        self.b().call_runtime(Runtime::ThrowIteratorResultNotAnObject, RegisterList { first: m, count: 1 });
        self.release(m);
        self.b().bind(&mut end);
    }

    /// Closes the iterator record unless done, on a throw: whatever `return` throws or gives is ignored
    pub fn iterator_close_quiet(&mut self, rec: Register) {
        let mut end = Label::default();
        self.b().ldar(Register(rec.0 + 2));
        self.b().jump(Bytecode::JumpIfTrue, &mut end);
        let _ = self.try_catch(j8bytecode::CatchPrediction::Caught, |c| {
            let mut skip = Label::default();
            c.b().get_named(rec, "return");
            c.b().jump(Bytecode::JumpIfUndefinedOrNull, &mut skip);
            let m = c.temp();
            c.b().star(m);
            c.b().call_property(m, RegisterList { first: rec, count: 1 });
            c.release(m);
            c.b().bind(&mut skip);
            Ok(())
        }, |_| Ok(()));
        self.b().bind(&mut end);
    }

    /// The left side of `for-of` / `for-in`: a declaration (including destructuring) or an assignment target, with the
    /// value in `v`.
    fn for_left(&mut self, left: &'a ForStatementLeft<'a>, v: Register) -> Result<(), Error> {
        match left {
            ForStatementLeft::VariableDeclaration(d) => {
                let decl = d.declarations.first().ok_or_else(|| self.unsupported(d.span, "an empty declaration"))?;
                self.bind_pattern(&decl.id, v, true)
            }
            t => self.assign_target(t.as_assignment_target().ok_or_else(|| self.unsupported(t.span(), "for-in / for-of target"))?, v),
        }
    }

    /// `for (left of right) body`: iterator protocol; `break` / `return` / a throw all close the iterator.
    fn for_of(&mut self, f: &'a ForOfStatement<'a>) -> Result<(), Error> {
        if f.r#await {
            return self.for_await(f);
        }
        let label = self.take_label();
        self.expr(&f.right)?;
        let src = self.temp();
        self.b().star(src);
        let iter = self.iterate(src);
        self.frame().controls.push(Control::Iterator(iter));
        let v = self.temp();
        let header = self.b().loop_header();
        self.frame().loop_depth += 1;
        let mut exit = Label::default();
        self.b().ldar(Register(iter.0 + 2));
        self.b().jump(Bytecode::JumpIfTrue, &mut exit);
        self.iterator_next(iter);
        self.b().star(v);
        self.b().ldar(Register(iter.0 + 2));
        self.b().jump(Bytecode::JumpIfTrue, &mut exit);
        self.push_jump(label, true, true);
        // A throw from the body closes the iterator and rethrows
        self.closing_on_throw(iter, |c| {
            c.scoped(&Decls::for_head(&f.left), |c| {
                c.for_left(&f.left, v)?;
                c.statement(&f.body)
            })
        })?;
        self.bind_continues();
        let depth = self.frame().loop_depth - 1;
        self.b().jump_loop(header, depth);
        self.frame().loop_depth -= 1;
        // break lands here: close the iterator
        self.pop_jump();
        self.iterator_close(iter);
        self.b().bind(&mut exit);
        self.frame().controls.pop();
        self.release(src);
        Ok(())
    }

    /// `for await (left of right) body` (not yet on V8 bytecode)
    fn for_await(&mut self, f: &'a ForOfStatement<'a>) -> Result<(), Error> {
        Err(self.unsupported(f.span, "for await"))
    }

    /// `for (left in right) body` (V8's `VisitForInStatement`: `ForInEnumerate` / `ForInPrepare`, then by index with
    /// `ForInNext`, skipping keys deleted meanwhile)
    fn for_in(&mut self, f: &'a ForInStatement<'a>) -> Result<(), Error> {
        let label = self.take_label();
        let mut exit = Label::default();
        self.expr(&f.right)?;
        self.b().jump(Bytecode::JumpIfUndefinedOrNull, &mut exit);
        let receiver = self.temp();
        self.b().to_object(receiver);
        self.b().for_in_enumerate(receiver);
        let cache = self.b().new_register_list(3);
        let slot = self.b().for_in_prepare(cache);
        let index = self.temp();
        self.b().load_smi(0);
        self.b().star(index);
        let v = self.temp();
        let header = self.b().loop_header();
        self.frame().loop_depth += 1;
        self.b().jump_if_for_in_done(&mut exit, index, cache.at(2));
        self.b().for_in_next(receiver, index, RegisterList { first: cache.first, count: 2 }, slot);
        self.push_jump(label, true, true);
        let mut skip = Label::default();
        self.b().jump(Bytecode::JumpIfUndefined, &mut skip);
        self.b().star(v);
        self.scoped(&Decls::for_head(&f.left), |c| {
            c.for_left(&f.left, v)?;
            c.statement(&f.body)
        })?;
        self.b().bind(&mut skip);
        self.bind_continues();
        self.b().for_in_step(index);
        let depth = self.frame().loop_depth - 1;
        self.b().jump_loop(header, depth);
        self.frame().loop_depth -= 1;
        self.pop_jump();
        self.b().bind(&mut exit);
        self.release(receiver);
        Ok(())
    }

    /// The generator's first suspension (after the prologue): not yet on V8 bytecode
    fn initial_yield(&mut self) -> Result<(), Error> {
        Err(self.unsupported(Span::default(), "generators"))
    }

    /// How many loops enclose the current position (the depth operand of a `JumpLoop` emitted here for a loop of its
    /// own)
    pub fn frame_loop_depth(&mut self) -> u32 {
        self.frame().loop_depth
    }

    pub fn is_async_generator(&mut self) -> bool {
        self.frame().async_generator
    }

    /// Replaces `value` with the default value if it is `undefined`: the result goes into a new temporary register (the
    /// caller frees it). `name` is the name inferred for the default value when it is an anonymous function.
    pub fn with_default(&mut self, value: Register, default: &'a Expression<'a>, name: Option<&'a str>) -> Result<Register, Error> {
        let t = self.temp();
        self.b().ldar(value);
        self.b().star(t);
        let mut skip = Label::default();
        self.b().jump(Bytecode::JumpIfNotUndefined, &mut skip);
        match name {
            Some(n) => self.expr_named(default, n)?,
            None => self.expr(default)?,
        }
        self.b().star(t);
        self.b().bind(&mut skip);
        Ok(t)
    }

    /// Destructuring `null` / `undefined` throws TypeError (for `{} = v`, where there is no property to read)
    pub fn require_object(&mut self, value: Register) {
        let mut ok = Label::default();
        self.b().ldar(value);
        self.b().jump(Bytecode::JumpIfUndefinedOrNull, &mut ok);
        let mut done = Label::default();
        self.b().jump(Bytecode::Jump, &mut done);
        self.b().bind(&mut ok);
        self.b().call_runtime(Runtime::ThrowPatternAssignmentNonCoercible, RegisterList { first: value, count: 1 });
        self.b().bind(&mut done);
    }

    /// Evaluates a property key into register `k` (a static key is a string constant).
    pub fn property_key_into(&mut self, key: &'a PropertyKey<'a>, computed: bool, k: Register) -> Result<(), Error> {
        match self.static_key(key, computed) {
            Some(c) => {
                let c = c?;
                self.b().load_constant(c);
            }
            None => {
                self.expr(key.as_expression().ok_or_else(|| self.unsupported(key.span(), "property key"))?)?;
                self.b().plain(Bytecode::ToName);
            }
        }
        self.b().star(k);
        Ok(())
    }

    /// Ordinary function (declaration or expression): rejects unsupported kinds.
    /// Ordinary function; an anonymous one uses the inferred name `inferred`.
    pub fn plain_function(&mut self, f: &'a AstFunction<'a>, self_name: bool, inferred: &str) -> Result<u32, Error> {
        let flavor = Flavor::from((f, Kind::Normal));
        let body = f.body.as_ref().ok_or_else(|| self.unsupported(f.span, "a function without a body"))?;
        let name = f.id.as_ref().map_or(inferred, |id| id.name.as_str());
        self.function(name, flavor, Some(Decls::this_key(f.span)), if self_name { f.id.as_ref() } else { None }, Some(&f.params), Body::Statements(&body.statements))
    }

    /// Method (class method, object literal method, accessor): not `new`able.
    pub fn method(&mut self, name: &str, f: &'a AstFunction<'a>, kind: Kind) -> Result<u32, Error> {
        let flavor = Flavor::from((f, kind));
        let body = f.body.as_ref().ok_or_else(|| self.unsupported(f.span, "a method without a body"))?;
        self.function(name, flavor, Some(Decls::this_key(f.span)), None, Some(&f.params), Body::Statements(&body.statements))
    }

    /// The body of a field initializer function (not yet on V8 bytecode)
    fn fields(&mut self, class: &'a Class<'a>, _statik: bool) -> Result<(), Error> {
        Err(self.unsupported(class.span, "class fields"))
    }

    /// The text of a non-computed property key (for function name inference; private names include `#`); `None` for
    /// computed keys.
    pub fn static_name(&self, key: &'a PropertyKey<'a>, computed: bool) -> Option<&'a str> {
        if computed {
            return None;
        }
        match key {
            PropertyKey::StaticIdentifier(id) => Some(id.name.as_str()),
            PropertyKey::StringLiteral(s) => s.value.as_str(),
            PropertyKey::NumericLiteral(n) => Some(self.alloc.alloc_str(ryu_js::Buffer::new().format(n.value))),
            PropertyKey::PrivateIdentifier(id) => Some(Hidden::private(self.alloc, id)),
            _ => None,
        }
    }

    /// Function name of a method / accessor: the key name, prefixed with `get ` / `set ` for accessors; empty for
    /// computed keys (known only at runtime).
    pub fn method_name(&self, key: &'a PropertyKey<'a>, computed: bool, kind: MethodDefinitionKind) -> &'a str {
        let Some(n) = self.static_name(key, computed) else { return "" };
        match kind {
            MethodDefinitionKind::Get => self.alloc.alloc_str(&format!("get {n}")),
            MethodDefinitionKind::Set => self.alloc.alloc_str(&format!("set {n}")),
            MethodDefinitionKind::Method | MethodDefinitionKind::Constructor => n,
        }
    }

    /// The constant for a non-computed property key (identifier / string / number); `None` for computed keys.
    pub fn static_key(&mut self, key: &'a PropertyKey<'a>, computed: bool) -> Option<Result<usize, Error>> {
        if computed {
            return None;
        }
        Some(match key {
            PropertyKey::StaticIdentifier(id) => Ok(self.name(id.name.as_str())),
            PropertyKey::StringLiteral(s) => match s.value.as_str() {
                Some(t) => Ok(self.name(t)),
                None => Err(self.unsupported(s.span, "lone surrogates")),
            },
            PropertyKey::NumericLiteral(n) => Ok(self.name(ryu_js::Buffer::new().format(n.value))),
            PropertyKey::PrivateIdentifier(id) => Err(self.unsupported(id.span, "private names")),
            _ => return None,
        })
    }

    /// Class: not yet on V8 bytecode (`DefineClass` with a class boilerplate). The result (the constructor) goes in acc.
    pub fn class(&mut self, c: &'a Class<'a>, _inferred: &'a str) -> Result<(), Error> {
        Err(self.unsupported(c.span, "classes"))
    }

    /// `super(...arguments)` of a default derived constructor (with classes)
    fn super_call_forward(&mut self) -> Result<(), Error> {
        Err(self.unsupported(Span::default(), "classes"))
    }

    fn function_declaration(&mut self, f: &'a AstFunction<'a>) -> Result<u32, Error> {
        self.plain_function(f, false, "")
    }

    /// Module body (not yet on V8 bytecode)
    fn module_body(&mut self, program: &'a Program<'a>) -> Result<(), Error> {
        Err(self.unsupported(program.span, "modules"))
    }

    /// The top level of a script or eval code (V8's `GenerateBytecodeBody` for a script / eval scope). A script's `var`s
    /// and function declarations are declared on the global object by `DeclareGlobals`, its `let` / `const` / `class`
    /// are slots of the script context the interpreter creates before running it (recorded as the function's scope).
    /// Sloppy eval code declares its `var`s / functions with `DeclareEvalVar` / `DeclareEvalFunction` in the calling
    /// variable environment; strict eval code keeps everything to itself. `local` is the eval code's own level.
    fn script(&mut self, program: &'a Program<'a>, local: Option<&Decls<'a>>, strict: bool, direct: bool) -> Result<(), Error> {
        self.module.push(SharedFunction { name: "".into(), kind: FunctionKind::NormalFunction, strict, length: 0, formal_parameters: 0, scope: None, bytecode: j8bytecode::BytecodeArray::default() });
        let eval = local.is_some();
        self.frames.push(Frame {
            index: 0,
            b: Builder::new(1, 0),
            name: "".into(),
            flavor: Flavor { strict, ..Flavor::default() },
            default_constructor: false,
            length: 0,
            formals: 0,
            scope: None,
            contexts: Vec::new(),
            loop_depth: 0,
            controls: Vec::new(),
            label: None,
            completion: None,
            generator: None,
            suspends: 0,
            async_generator: false,
            strict,
            annexb: Vec::new(),
            var_level: None,
            eval: eval && direct,
            prologue: false,
            simple_params: false,
        });
        let completion = self.temp();
        self.frame().completion = Some(completion);
        let mut rw = Rewriter::default();
        rw.process(program.body.iter());
        self.completion_assign = rw.assign;
        self.completion_reset = rw.reset;
        // `this` is the receiver (the global object) for a script and indirect eval code; direct eval code looks it up
        // by name (functions with a direct eval keep `this` in their context, as V8 does)
        if !(eval && direct) {
            self.enter(&Decls(vec![Decl { name: THIS, key: Decls::this_key(program.span), kind: DeclKind::This, ready: None }]), ScopeType::ScriptScope)?;
        }
        let mut globals = Decls::default();
        if !(eval && strict) {
            globals.vars(&program.body);
            globals.lexical(&program.body);
        }
        // annexB: block-level functions in sloppy code also create a `var` in the variable environment
        if !strict {
            let hoisted = Decls::annexb_functions(&program.body, None);
            self.frame().annexb = hoisted.iter().map(|f| f.span.start).collect();
            globals.annexb_globals(&hoisted);
        }
        if eval {
            if let Some(d) = local {
                self.enter(d, ScopeType::EvalScope)?;
            }
            if !strict {
                for d in &globals.0 {
                    match d.kind {
                        DeclKind::Var => {
                            let r = self.temp();
                            self.b().load_string(d.name);
                            self.b().star(r);
                            self.b().call_runtime(Runtime::DeclareEvalVar, RegisterList { first: r, count: 1 });
                            self.release(r);
                        }
                        DeclKind::Function(f) => {
                            let n = self.function_declaration(f)?;
                            let args = self.b().new_register_list(2);
                            self.b().load_string(d.name);
                            self.b().star(args.first);
                            self.closure(n);
                            self.b().star(args.at(1));
                            self.b().call_runtime(Runtime::DeclareEvalFunction, args);
                            self.release(args.first);
                        }
                        _ => {}
                    }
                }
            }
        } else {
            // Script: `var`s and functions on the global object in one `DeclareGlobals`; lexical declarations in the
            // script context
            let mut declarations = Vec::new();
            let mut variables = Vec::new();
            let mut lexical = Vec::new();
            for d in &globals.0 {
                match d.kind {
                    DeclKind::Var => declarations.push(GlobalDeclaration { name: d.name.into(), function: None }),
                    DeclKind::Function(f) => {
                        let n = self.function_declaration(f)?;
                        declarations.retain(|g: &GlobalDeclaration| *g.name != *d.name);
                        declarations.push(GlobalDeclaration { name: d.name.into(), function: Some(n) });
                    }
                    DeclKind::Let | DeclKind::Const => {
                        let slot = Context::MIN_SLOTS + u32::try_from(variables.len()).unwrap_or(u32::MAX);
                        let mode = if matches!(d.kind, DeclKind::Const) { VariableMode::Const } else { VariableMode::Let };
                        variables.push(ContextVariable { name: d.name.into(), mode });
                        lexical.push(Bound { name: d.name, binding: Binding::Context(slot), constant: matches!(d.kind, DeclKind::Const), ready: d.ready.or(Some(u32::MAX)), self_name: false });
                    }
                    _ => {}
                }
            }
            if !variables.is_empty() {
                self.frame().scope = Some(ScopeInfo { scope_type: Some(ScopeType::ScriptScope), variables, has_extension: false });
            }
            // The script context is already current: this level doesn't push or pop it
            let mark = self.b().mark();
            self.levels.push(Level { func: 0, names: lexical, context: None, scope: None, dynamic: false, mark, switch: false, cells: true });
            if !declarations.is_empty() {
                let k = self.constant(Constant::Declarations(declarations));
                let args = self.b().new_register_list(2);
                self.b().load_constant(k);
                self.b().star(args.first);
                self.b().mov(Register::function_closure(), args.at(1));
                self.b().call_runtime(Runtime::DeclareGlobals, args);
                self.release(args.first);
            }
        }
        // The directive prologue (`'use strict'` etc.) is not in body in oxc, but it consists of expression statements
        // and counts toward the completion value
        for d in &program.directives {
            let k = self.string_const(d.expression.value.as_str(), d.expression.span)?;
            self.b().load_constant(k);
            self.b().star(completion);
        }
        self.statements(&program.body)?;
        self.b().ldar(completion);
        self.b().plain(Bytecode::Return);
        let f = self.frames.pop().expect("the script's frame");
        self.levels.clear();
        self.module[0] = f.finish();
        Ok(())
    }

    // ---- Statements --------------------------------------------------------------------

    fn statements(&mut self, stmts: &'a [Statement<'a>]) -> Result<(), Error> {
        stmts.iter().try_for_each(|s| self.statement(s))
    }

    fn push_jump(&mut self, label: Option<&'a str>, breakable: bool, continuable: bool) {
        let f = self.frame();
        let contexts = f.contexts.clone();
        f.controls.push(Control::Jump(Jump { label, breakable, continuable, breaks: Label::default(), continues: Label::default(), contexts }));
    }

    /// The innermost jump target's `continue`s land here
    fn bind_continues(&mut self) {
        let f = self.frame();
        let Some(Control::Jump(j)) = f.controls.iter_mut().rev().find(|c| matches!(c, Control::Jump(_))) else { unreachable!("a loop is the innermost jump target") };
        let mut l = core::mem::take(&mut j.continues);
        f.b.bind(&mut l);
    }

    /// Pops the innermost jump target: its `break`s land here
    fn pop_jump(&mut self) {
        let Some(Control::Jump(mut j)) = self.frame().controls.pop() else { unreachable!("the innermost control is this jump target") };
        self.b().bind(&mut j.breaks);
    }

    /// A loop / `switch` consumes the pending labels.
    fn take_label(&mut self) -> Option<&'a str> {
        self.frame().label.take()
    }

    /// Finds where a jump out lands, innermost first: detours into the first finally encountered; `target` of `None`
    /// means `return`.
    fn exit_plan(&self, target: Option<(Option<&'a str>, bool)>) -> Exit {
        let f = self.frames.last().expect("inside a function");
        for (i, c) in f.controls.iter().enumerate().rev() {
            match c {
                Control::Finally(_) => return Exit::Finally(i),
                Control::Jump(j) => {
                    let hit = match target {
                        Some((Some(l), false)) => j.label == Some(l),
                        Some((Some(l), true)) => j.label == Some(l) && j.continuable,
                        Some((None, false)) => j.breakable,
                        Some((None, true)) => j.continuable,
                        None => false,
                    };
                    if hit {
                        return Exit::Jump(i);
                    }
                }
                Control::Handler | Control::Iterator(_) | Control::AsyncIterator(_) => {}
            }
        }
        Exit::Function
    }

    /// Makes the context the one of a landing point that had `contexts` pushed (one `PopContext` to the saved outer
    /// context of the first extra one)
    fn restore_contexts(&mut self, contexts: &[Register]) {
        let cur = self.frame().contexts.clone();
        if cur.len() > contexts.len() {
            self.b().pop_context(cur[contexts.len()]);
        }
    }

    /// Leaves control stack levels `from` and above, innermost first: closes iterators passed. acc is kept.
    fn leave_controls(&mut self, from: usize) -> Result<(), Error> {
        let iters: Vec<Control<'a>> = self.frame().controls[from..]
            .iter()
            .rev()
            .filter_map(|c| match c {
                Control::Iterator(i) => Some(Control::Iterator(*i)),
                Control::AsyncIterator(i) => Some(Control::AsyncIterator(*i)),
                _ => None,
            })
            .collect();
        if iters.is_empty() {
            return Ok(());
        }
        let saved = self.temp();
        self.b().star(saved);
        for c in iters {
            match c {
                Control::Iterator(iter) => self.iterator_close(iter),
                _ => return Err(self.unsupported(Span::default(), "for await")),
            }
        }
        self.b().ldar(saved);
        self.release(saved);
        Ok(())
    }

    /// Jumps into the finally of level `i`, carrying continuation token `token`.
    fn enter_finally(&mut self, i: usize, token: i32) -> Result<(), Error> {
        let Control::Finally(fin) = &self.frame().controls[i] else { unreachable!("a finally") };
        let (contexts, token_reg) = (fin.contexts.clone(), fin.token);
        self.leave_controls(i + 1)?;
        self.restore_contexts(&contexts);
        let saved = self.temp();
        self.b().star(saved);
        self.b().load_smi(token);
        self.b().star(token_reg);
        self.b().ldar(saved);
        self.release(saved);
        let f = self.frame();
        let Control::Finally(fin) = &mut f.controls[i] else { unreachable!("a finally") };
        let mut l = core::mem::take(&mut fin.entry);
        f.b.jump(Bytecode::Jump, &mut l);
        let Control::Finally(fin) = &mut f.controls[i] else { unreachable!("a finally") };
        fin.entry = l;
        Ok(())
    }

    /// `break` / `continue`: restores the context, closes the iterators passed, then jumps; detours into any finally
    /// in between first.
    fn exit(&mut self, span: Span, label: Option<&'a str>, cont: bool) -> Result<(), Error> {
        match self.exit_plan(Some((label, cont))) {
            Exit::Jump(i) => {
                let Control::Jump(j) = &self.frame().controls[i] else { unreachable!("a jump target") };
                let contexts = j.contexts.clone();
                self.leave_controls(i + 1)?;
                self.restore_contexts(&contexts);
                let f = self.frame();
                let Control::Jump(j) = &mut f.controls[i] else { unreachable!("a jump target") };
                let mut l = core::mem::take(if cont { &mut j.continues } else { &mut j.breaks });
                f.b.jump(Bytecode::Jump, &mut l);
                let Control::Jump(j) = &mut f.controls[i] else { unreachable!("a jump target") };
                if cont { j.continues = l } else { j.breaks = l }
                Ok(())
            }
            Exit::Finally(i) => {
                let Control::Finally(fin) = &mut self.frame().controls[i] else { unreachable!("a finally") };
                let n = fin.routes.len();
                fin.routes.push(Route { span, label, cont });
                self.enter_finally(i, COMPLETION_ROUTE + i32::try_from(n).expect("fewer than 2^31 routes"))
            }
            Exit::Function => Err(self.unsupported(span, "a jump with no target")),
        }
    }

    /// `return` (value in acc): if a finally is in between, stores the value and detours into it first; closes all
    /// iterators passed.
    pub fn ret(&mut self) {
        match self.exit_plan(None) {
            Exit::Finally(i) => {
                let Control::Finally(fin) = &self.frame().controls[i] else { unreachable!("a finally") };
                let value = fin.value;
                self.b().star(value);
                let _ = self.enter_finally(i, COMPLETION_RETURN);
            }
            _ => {
                let _ = self.leave_controls(0);
                self.b().plain(Bytecode::Return);
            }
        }
    }

    /// Branches of `if`: a bare function declaration is treated as a block containing only it (annexB B.3.3).
    fn clause(&mut self, s: &'a Statement<'a>) -> Result<(), Error> {
        if matches!(s, Statement::FunctionDeclaration(_)) {
            let one = core::slice::from_ref(s);
            self.scoped(&Decls::block([one]), |c| c.statements(one))
        } else {
            self.statement(s)
        }
    }

    fn block(&mut self, b: &'a BlockStatement<'a>) -> Result<(), Error> {
        self.scoped(&Decls::block([b.body.as_slice()]), |c| c.statements(&b.body))
    }

    /// Runs `then` when the continuation token in `reg` is `token`.
    pub fn on_completion(&mut self, reg: Register, token: i32, then: impl FnOnce(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        self.b().load_smi(token);
        self.b().compare(CompareOp::StrictEq, reg);
        let mut skip = Label::default();
        self.b().jump(Bytecode::JumpIfFalse, &mut skip);
        then(self)?;
        self.b().bind(&mut skip);
        Ok(())
    }

    /// The current level gets a fresh copy of its context (a `for (let …)` iteration: closures from the previous
    /// iteration keep the old one)
    fn renew_context(&mut self) {
        let Some(level) = self.levels.last() else { return };
        let Some(saved) = level.context else { return };
        let slots: Vec<u32> = level.names.iter().filter_map(|n| match n.binding {
            Binding::Context(s) => Some(s),
            _ => None,
        }).collect();
        let Some(scope) = level.scope else { return };
        let n = u32::try_from(slots.len()).unwrap_or(0);
        let tmp = self.b().new_register_list(n);
        for (i, &s) in (0u32..).zip(&slots) {
            self.b().load_context_slot(Register::current_context(), s, 0, false, false);
            self.b().star(tmp.at(i));
        }
        self.b().pop_context(saved);
        self.b().create_block_context(scope);
        self.b().push_context(saved);
        for (i, &s) in (0u32..).zip(&slots) {
            self.b().ldar(tmp.at(i));
            self.b().store_context_slot(Register::current_context(), s, 0);
        }
        self.release(tmp.first);
    }

    /// `try` / `catch` / `finally` (V8's `TryCatchBuilder` / `TryFinallyBuilder`: handler table regions; a finally is
    /// entered with a continuation token and dispatches on it at the end)
    fn try_statement(&mut self, t: &'a TryStatement<'a>) -> Result<(), Error> {
        let fin = t.finalizer.as_ref().map(|_| {
            let token = self.temp();
            let value = self.temp();
            let contexts = self.frame().contexts.clone();
            self.frame().controls.push(Control::Finally(Finally { token, value, contexts, entry: Label::default(), routes: Vec::new() }));
            let ctx = self.temp();
            self.b().mov(Register::current_context(), ctx);
            let h = self.b().new_handler(ctx, j8bytecode::CatchPrediction::Uncaught);
            self.b().try_start(h);
            self.frame().controls.push(Control::Handler);
            (token, value, h)
        });
        match &t.handler {
            Some(c) => self.try_catch(j8bytecode::CatchPrediction::Caught, |s| s.block(&t.block), |s| {
                s.scoped(&Decls::catch_param(c), |s| {
                    match &c.param {
                        Some(p) => {
                            let e = s.temp();
                            s.b().star(e);
                            s.bind_pattern(&p.pattern, e, true)?;
                            s.release(e);
                        }
                        None => {}
                    }
                    s.block(&c.body)
                })
            })?,
            None => self.block(&t.block)?,
        }
        let Some((token, value, h)) = fin else { return Ok(()) };
        self.frame().controls.pop();
        self.b().try_end(h);
        let mut fall = Label::default();
        self.b().load_smi(COMPLETION_NORMAL);
        self.b().star(token);
        self.b().jump(Bytecode::Jump, &mut fall);
        // Entered by a throw: keep the exception for the rethrow
        self.b().handler_target(h);
        self.b().star(value);
        self.b().load_smi(COMPLETION_THROW);
        self.b().star(token);
        self.b().bind(&mut fall);
        let Some(Control::Finally(mut f)) = self.frame().controls.pop() else { unreachable!("the innermost control is this finally") };
        self.b().bind(&mut f.entry);
        self.block(t.finalizer.as_ref().expect("checked above"))?;
        self.on_completion(token, COMPLETION_THROW, |s| {
            s.b().ldar(value);
            s.b().plain(Bytecode::ReThrow);
            Ok(())
        })?;
        self.on_completion(token, COMPLETION_RETURN, |s| {
            s.b().ldar(value);
            s.ret();
            Ok(())
        })?;
        for (n, r) in (0i32..).zip(f.routes) {
            self.on_completion(token, COMPLETION_ROUTE + n, |s| s.exit(r.span, r.label, r.cont))?;
        }
        self.release(token);
        Ok(())
    }

    /// A loop's back edge: `continue`s land here, then `JumpLoop` to `header`
    fn loop_back(&mut self, header: crate::builder::LoopHeader) {
        self.bind_continues();
        let depth = self.frame().loop_depth - 1;
        self.b().jump_loop(header, depth);
    }

    #[expect(clippy::too_many_lines, reason = "one arm per statement kind; splitting it would hurt readability")]
    fn statement(&mut self, s: &'a Statement<'a>) -> Result<(), Error> {
        // The completion value resets to `undefined` before a statement that may not produce one (V8's Rewriter)
        if let Some(c) = self.frame().completion
            && self.completion_reset.contains(&s.span().start)
        {
            self.b().load_undefined();
            self.b().star(c);
        }
        match s {
            Statement::ExpressionStatement(e) => match self.frame().completion.filter(|_| self.completion_assign.contains(&e.span.start)) {
                Some(c) => {
                    self.expr(&e.expression)?;
                    self.b().star(c);
                }
                None => self.effect(&e.expression)?,
            },
            Statement::VariableDeclaration(d) => self.declaration(d)?,
            Statement::FunctionDeclaration(f) => self.annexb_copy(f),
            Statement::EmptyStatement(_) => {}
            Statement::ClassDeclaration(c) => {
                self.class(c, "")?;
                if let Some(id) = &c.id {
                    self.store(id.name.as_str(), true);
                }
            }
            Statement::BlockStatement(b) => self.block(b)?,
            Statement::ReturnStatement(r) => {
                match &r.argument {
                    Some(e) => self.expr(e)?,
                    None => self.b().load_undefined(),
                }
                self.ret();
            }
            Statement::TryStatement(t) => self.try_statement(t)?,
            Statement::ThrowStatement(t) => {
                self.expr(&t.argument)?;
                self.b().plain(Bytecode::Throw);
            }
            Statement::IfStatement(i) => {
                self.expr(&i.test)?;
                let mut skip = Label::default();
                self.b().jump(Bytecode::JumpIfToBooleanFalse, &mut skip);
                self.clause(&i.consequent)?;
                if let Some(a) = &i.alternate {
                    let mut end = Label::default();
                    self.b().jump(Bytecode::Jump, &mut end);
                    self.b().bind(&mut skip);
                    self.clause(a)?;
                    self.b().bind(&mut end);
                } else {
                    self.b().bind(&mut skip);
                }
            }
            Statement::WhileStatement(w) => {
                let label = self.take_label();
                let header = self.b().loop_header();
                self.frame().loop_depth += 1;
                self.push_jump(label, true, true);
                self.expr(&w.test)?;
                let mut exit = Label::default();
                self.b().jump(Bytecode::JumpIfToBooleanFalse, &mut exit);
                self.statement(&w.body)?;
                self.loop_back(header);
                self.frame().loop_depth -= 1;
                self.b().bind(&mut exit);
                self.pop_jump();
            }
            Statement::DoWhileStatement(w) => {
                let label = self.take_label();
                let header = self.b().loop_header();
                self.frame().loop_depth += 1;
                self.push_jump(label, true, true);
                self.statement(&w.body)?;
                self.bind_continues();
                self.expr(&w.test)?;
                let mut exit = Label::default();
                self.b().jump(Bytecode::JumpIfToBooleanFalse, &mut exit);
                let depth = self.frame().loop_depth - 1;
                self.b().jump_loop(header, depth);
                self.frame().loop_depth -= 1;
                self.b().bind(&mut exit);
                self.pop_jump();
            }
            Statement::ForStatement(f) => {
                let label = self.take_label();
                let init_text = match &f.init {
                    Some(ForStatementInit::VariableDeclaration(d)) => {
                        let inits: Vec<&str> = d.declarations.iter().filter_map(|v| v.init.as_ref()).map(|e| &self.src.text[e.span().start as usize..e.span().end as usize]).collect();
                        Some(self.alloc.alloc_str(&inits.join(" ")))
                    }
                    _ => None,
                };
                self.for_init_text = init_text.map(|t| &*t);
                let decls = Decls::for_init(f.init.as_ref());
                self.enter(&decls, ScopeType::BlockScope)?;
                self.for_init_text = None;
                let r = (|c: &mut Self| -> Result<(), Error> {
                    match &f.init {
                        Some(ForStatementInit::VariableDeclaration(d)) => c.declaration(d)?,
                        Some(init) => c.effect(init.as_expression().expect("a for-init is a declaration or an expression"))?,
                        None => {}
                    }
                    let header = c.b().loop_header();
                    c.frame().loop_depth += 1;
                    c.push_jump(label, true, true);
                    let mut exit = Label::default();
                    if let Some(t) = &f.test {
                        c.expr(t)?;
                        c.b().jump(Bytecode::JumpIfToBooleanFalse, &mut exit);
                    }
                    c.statement(&f.body)?;
                    c.bind_continues();
                    // A fresh `let` context per iteration; closures from the previous iteration keep the old one
                    c.renew_context();
                    if let Some(u) = &f.update {
                        c.effect(u)?;
                    }
                    let depth = c.frame().loop_depth - 1;
                    c.b().jump_loop(header, depth);
                    c.frame().loop_depth -= 1;
                    c.b().bind(&mut exit);
                    c.pop_jump();
                    Ok(())
                })(self);
                r?;
                self.leave();
            }
            Statement::WithStatement(w) => {
                self.expr(&w.object)?;
                let object = self.temp();
                self.b().to_object(object);
                let k = self.constant(Constant::Scope(ScopeInfo { scope_type: Some(ScopeType::WithScope), variables: Vec::new(), has_extension: false }));
                self.b().create_with_context(object, k);
                let saved = self.temp();
                self.b().push_context(saved);
                self.frame().contexts.push(saved);
                let mark = self.b().mark();
                let func = self.frames.len() - 1;
                self.levels.push(Level { func, names: Vec::new(), context: Some(saved), scope: None, dynamic: true, mark, switch: false, cells: false });
                self.statement(&w.body)?;
                self.leave();
                self.release(object);
            }
            Statement::LabeledStatement(l) => {
                let name = l.label.name.as_str();
                if matches!(l.body, Statement::WhileStatement(_) | Statement::DoWhileStatement(_) | Statement::ForStatement(_) | Statement::ForOfStatement(_) | Statement::ForInStatement(_) | Statement::SwitchStatement(_)) {
                    self.frame().label = Some(name);
                    self.statement(&l.body)?;
                } else {
                    self.push_jump(Some(name), false, false);
                    self.statement(&l.body)?;
                    self.pop_jump();
                }
            }
            Statement::BreakStatement(b) => self.exit(b.span, b.label.as_ref().map(|l| l.name.as_str()), false)?,
            Statement::ContinueStatement(c) => self.exit(c.span, c.label.as_ref().map(|l| l.name.as_str()), true)?,
            Statement::SwitchStatement(sw) => self.switch(sw)?,
            Statement::ForOfStatement(f) => self.for_of(f)?,
            Statement::ForInStatement(f) => self.for_in(f)?,
            s => return Err(self.unsupported(s.span(), "statement")),
        }
        Ok(())
    }

    fn declaration(&mut self, d: &'a VariableDeclaration<'a>) -> Result<(), Error> {
        if !matches!(d.kind, VariableDeclarationKind::Var | VariableDeclarationKind::Let | VariableDeclarationKind::Const) {
            return Err(self.unsupported(d.span, "`using` declarations"));
        }
        for v in &d.declarations {
            let BindingPattern::BindingIdentifier(id) = &v.id else {
                let init = v.init.as_ref().ok_or_else(|| self.unsupported(v.span, "a destructuring declaration without an initializer"))?;
                self.expr(init)?;
                let t = self.temp();
                self.b().star(t);
                self.bind_pattern(&v.id, t, true)?;
                self.release(t);
                continue;
            };
            match (&v.init, d.kind) {
                (Some(e), _) => self.expr_named(e, id.name.as_str())?,
                // `let x;` in a loop body must reset to undefined every iteration
                (None, VariableDeclarationKind::Let) => self.b().load_undefined(),
                (None, _) => continue,
            }
            self.store(id.name.as_str(), true);
        }
        Ok(())
    }

    /// `switch` (V8's `VisitSwitchStatement`): compares each `case` with strict equality to jump into its body; bodies
    /// fall through in order.
    fn switch(&mut self, sw: &'a SwitchStatement<'a>) -> Result<(), Error> {
        let label = self.take_label();
        self.expr(&sw.discriminant)?;
        let d = self.temp();
        self.b().star(d);
        self.scoped(&Decls::block(sw.cases.iter().map(|c| c.consequent.as_slice())), |c| {
            c.levels.last_mut().expect("the switch scope").switch = true;
            let mut entries: Vec<Option<Label>> = Vec::with_capacity(sw.cases.len());
            for case in &sw.cases {
                if let Some(t) = &case.test {
                    c.expr(t)?;
                    c.b().compare(CompareOp::StrictEq, d);
                    let mut l = Label::default();
                    c.b().jump(Bytecode::JumpIfTrue, &mut l);
                    entries.push(Some(l));
                } else {
                    entries.push(None);
                }
            }
            let mut fallback = Label::default();
            c.b().jump(Bytecode::Jump, &mut fallback);
            c.push_jump(label, true, false);
            let mut has_default = false;
            for (case, entry) in sw.cases.iter().zip(entries) {
                match entry {
                    Some(mut l) => c.b().bind(&mut l),
                    None => {
                        has_default = true;
                        c.b().bind(&mut fallback);
                    }
                }
                c.statements(&case.consequent)?;
            }
            if !has_default {
                c.b().bind(&mut fallback);
            }
            c.pop_jump();
            Ok(())
        })?;
        self.release(d);
        Ok(())
    }
}
