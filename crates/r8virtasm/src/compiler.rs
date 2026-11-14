//! JS ⇒ bytecode: scopes, functions, statements. Expressions are in `expr.rs`.
use std::collections::HashSet;

use oxc_allocator::Allocator;
use oxc_ast::ast::{Directive, BindingIdentifier, BindingPattern, BlockStatement, Class, ClassElement, Expression, MethodDefinition, MethodDefinitionKind, PrivateIdentifier, PropertyKey, ForStatementInit, FormalParameters, Function as AstFunction, ForInStatement, ForOfStatement, ForStatementLeft, Program, Statement, SwitchStatement, TryStatement, VariableDeclaration, VariableDeclarationKind};
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, SourceType, Span};
use r8framework::{CodeKind, CompareOp, Error, ImportEntry, ImportName, ModuleInfo, Resume, Source};

use crate::scope::{Decl, DeclKind, Decls, Hidden, Key, Resolver, THIS};
use crate::{Const, Constant, EvalName, EvalScope, Function, FunctionKind, Insn, Label, Module, PropertyKind, Reg};

/// Jump target before backpatching
const UNPATCHED: Label = Label(u32::MAX);

/// finally completion type: normal fall-through
const COMPLETION_NORMAL: f64 = 0.0;
/// finally completion type: entered with an exception, rethrows on exit
const COMPLETION_THROW: f64 = 1.0;
/// finally completion type: entered with a return value, keeps returning on exit
const COMPLETION_RETURN: f64 = 2.0;
/// finally completion type: the n-th detoured break / continue is this number plus n
const COMPLETION_ROUTE: f64 = 3.0;

/// Where a name is bound.
#[derive(Clone, Copy)]
enum Binding {
    Reg(Reg),
    /// A slot in this level's environment
    Env(u16),
    /// The module's n-th import binding (at runtime, read via the module record from the exporter's slot)
    Import(u16),
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

/// Parameter i ⇒ its slot in the function-level environment (for duplicate parameter names, the last one wins).
fn alloc_vec_slots(decls: &Decls<'_>, level: &Level<'_>) -> Vec<Option<u16>> {
    let mut slots: Vec<Option<u16>> = Vec::new();
    for (d, n) in decls.0.iter().zip(&level.names) {
        if let (DeclKind::Param(i), Binding::Env(slot)) = (d.kind, n.binding) {
            let i = usize::from(i);
            if slots.len() <= i {
                slots.resize(i + 1, None);
            }
            slots[i] = Some(slot);
        }
    }
    slots
}

/// Binding name for `export default expression` / an anonymous default export
const DEFAULT_BINDING: &str = "*default*";

/// One scope level during code generation.
struct Level<'a> {
    /// Which function level it belongs to (index into `frames`)
    func: usize,
    names: Vec<Bound<'a>>,
    /// Whether this level has a heap environment
    env: bool,
    /// The register stack top before entering, restored on exit
    mark: u16,
    /// The level of a `switch`: cases may skip over declarations, so TDZ checks cannot be elided
    switch: bool,
}

/// Result of name resolution: where it is, whether it is constant, and whether reads / writes need a TDZ check (giving
/// the position where the declaration ends).
#[derive(Clone, Copy)]
pub(crate) struct Resolved<'a> {
    pub place: Place<'a>,
    pub constant: bool,
    /// `Some(ready)`: a check is needed if the use is before `ready`, or the binding is in an environment / in a
    /// `switch`
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
pub(crate) enum Place<'a> {
    Reg(Reg),
    Env { depth: u16, slot: u16 },
    Global(&'a str),
    Import(u16),
}

/// Where a `break` / `continue` goes.
struct Jump<'a> {
    label: Option<&'a str>,
    /// Whether an unlabeled `break` can exit it (loops and `switch` can, labeled blocks cannot)
    breakable: bool,
    continuable: bool,
    breaks: Vec<usize>,
    continues: Vec<usize>,
    /// How many environment levels this function had on entering it: jumping out must pop the extra ones
    envs: u16,
}

/// A `break` / `continue` passing through a finally: the jump continues after the finally runs.
#[derive(Clone, Copy)]
struct Route<'a> {
    span: Span,
    label: Option<&'a str>,
    cont: bool,
}

/// A finally: registers for the completion type and value, where it is entered, and the detoured jumps.
struct Finally<'a> {
    kind: Reg,
    value: Reg,
    envs: u16,
    entries: Vec<usize>,
    routes: Vec<Route<'a>>,
}

/// One level of the control stack: jump targets, an installed exception handler, a finally.
enum Control<'a> {
    Jump(Jump<'a>),
    Handler,
    Finally(Finally<'a>),
    /// The iterator of a `for-of` (its record is in these three registers): must be closed before jumping out
    Iterator(Reg),
    /// The iterator of a `for await` (its record is in these four registers): jumping out must call `return` and await
    /// it first
    AsyncIterator(Reg),
}

/// Function kind and flavor (generator, async).
#[derive(Clone, Copy, Default)]
pub(crate) struct Flavor {
    pub kind: FunctionKind,
    pub generator: bool,
    pub asynchronous: bool,
    /// Strict on its own (`'use strict'` at the start of the body, inside a class body); strictness of an enclosing
    /// scope is inherited regardless
    pub strict: bool,
}

/// Flavor of a function node: generator / async from the node, kind given by the caller.
impl From<(&AstFunction<'_>, FunctionKind)> for Flavor {
    fn from((f, kind): (&AstFunction<'_>, FunctionKind)) -> Self {
        let strict = f.body.as_ref().is_some_and(|b| Self::use_strict(&b.directives));
        Self { kind, generator: f.generator, asynchronous: f.r#async, strict }
    }
}

impl Flavor {
    /// Whether the directive prologue contains `'use strict'` (written exactly so, without escapes).
    pub fn use_strict(directives: &[Directive<'_>]) -> bool {
        directives.iter().any(|d| d.directive.as_str() == "use strict")
    }
}

impl From<FunctionKind> for Flavor {
    fn from(kind: FunctionKind) -> Self {
        Self { kind, ..Self::default() }
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
    out: Function,
    /// The next free register
    top: u16,
    /// How many environment levels of this function are currently active
    envs: u16,
    controls: Vec<Control<'a>>,
    /// The label for the next loop / `switch`
    label: Option<&'a str>,
    /// The script's completion value goes here (only the script itself has one)
    completion: Option<Reg>,
    /// The register a generator / async function writes the resume mode to when resumed
    resume: Option<Reg>,
    /// Async generator: `yield` / `return` await the value first
    async_generator: bool,
    /// Strict mode
    strict: bool,
    /// annexB: block-level functions whose value must be written to the outer `var` when execution reaches the
    /// declaration (by start position of the function node)
    annexb: Vec<u32>,
    /// Index of the function-level scope (where `var`s live); global for a script
    var_level: Option<usize>,
    /// Direct eval code: the object in the calling function that holds eval vars (annexB block-level functions write
    /// here)
    eval_vars: Option<&'a str>,
    /// Compiling the parameter prologue (an eval here cannot see the body's lexical declarations)
    prologue: bool,
    /// All parameters are simple names (no default values, destructuring, or rest)
    simple_params: bool,
}

/// A function body: statements, an arrow function's expression, class field initializers, a default constructor.
#[derive(Clone, Copy)]
pub(crate) enum Body<'a> {
    Statements(&'a [Statement<'a>]),
    Expression(&'a Expression<'a>),
    /// Instance (`statik` false) or static fields of a class: defined one by one on `this`
    Fields { class: &'a Class<'a>, statik: bool },
    /// A class without a `constructor`: a derived class forwards `super(...arguments)`
    DefaultConstructor { derived: bool },
}

pub(crate) struct Compiler<'a, 's> {
    src: &'s Source<'s>,
    /// The AST allocator: hidden binding names in class scopes (`#x` etc.) are allocated here too
    pub(crate) alloc: &'a Allocator,
    /// Referenced bindings (`new.target` / `arguments` are prepared only if used)
    used: HashSet<Key>,
    /// Number of tagged template call sites (template objects are cached by (function, number))
    pub(crate) sites: u32,
    /// How many class bodies are being compiled (a class body is entirely strict code)
    class_depth: u32,
    captured: HashSet<Key>,
    module: Vec<Function>,
    frames: Vec<Frame<'a>>,
    levels: Vec<Level<'a>>,
    /// Top-level `let` / `const` / `class` of a script: in the script scope, initialized via `InitLexical`
    script_lexicals: HashSet<&'a str>,
    /// Optional chains being compiled: in each chain, `?.` jumps to the end of the chain on null / undefined
    pub(crate) chains: Vec<Vec<usize>>,
    /// Functions containing a direct eval (by start position of the parameter list)
    eval_fns: HashSet<u32>,
}

impl TryFrom<&Source<'_>> for Module {
    type Error = Error;

    /// Compiles a script: the value of the last expression statement is the return value.
    fn try_from(src: &Source<'_>) -> Result<Self, Error> {
        Self::compile(src, CodeKind::Script)
    }
}

impl Module {
    /// Compiles by kind: a script's top-level lexical declarations go into the script scope; eval code's stay within
    /// itself.
    pub fn compile(src: &Source<'_>, kind: CodeKind) -> Result<Self, Error> {
        Self::compile_in(src, kind, None)
    }

    /// Direct eval code: `outer` is the bindings visible at the call site (at runtime the eval code's environment hangs
    /// off the call site's environment chain).
    pub fn compile_direct(src: &Source<'_>, strict: bool, outer: &EvalScope) -> Result<Self, Error> {
        Self::compile_in(src, if strict { CodeKind::StrictEval } else { CodeKind::Eval }, Some(outer))
    }

    fn compile_in(src: &Source<'_>, kind: CodeKind, outer: Option<&EvalScope>) -> Result<Self, Error> {
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
        // Functions containing a direct eval: all their bindings go into environments, and sloppy ones get an extra
        // object holding eval `var`s
        let scoping = checked.semantic.scoping();
        let eval_scopes: HashSet<oxc_syntax::scope::ScopeId> = scoping.scope_descendants_from_root().filter(|&id| scoping.scope_flags(id).contains_direct_eval()).collect();
        let outer_names: Option<Vec<Vec<&str>>> = outer.map(|o| o.0.iter().map(|l| l.iter().map(|n| allocator.alloc_str(&n.name)).collect()).collect());
        let strict = kind == CodeKind::StrictEval || Flavor::use_strict(&program.directives);
        let local = (kind != CodeKind::Script).then(|| Decls::eval_local(&program.body, strict));
        let (captured, used, eval_fns) = Resolver::script(&program.body, Decls::this_key(program.span), &allocator, local.as_ref(), strict, eval_scopes, outer_names.as_deref());
        let mut c = Compiler { src, alloc: &allocator, captured, used, sites: 0, class_depth: 0, module: Vec::new(), frames: Vec::new(), levels: Vec::new(), script_lexicals: HashSet::new(), chains: Vec::new(), eval_fns };
        if let (Some(o), Some(names)) = (outer, &outer_names) {
            // The call site's environment chain: pushed outermost first as scope levels (all in environments, slots
            // copied as is)
            for (level, names) in o.0.iter().zip(names).rev() {
                let bound = level.iter().zip(names).map(|(n, &name)| Bound { name, binding: Binding::Env(n.slot), constant: n.constant, ready: n.tdz.then_some(u32::MAX), self_name: n.self_name }).collect();
                c.levels.push(Level { func: usize::MAX, names: bound, env: true, mark: 0, switch: false });
            }
        }
        c.script(program, local.as_ref(), strict)?;
        c.module.iter_mut().for_each(Function::fuse);
        Ok(Module { functions: c.module })
    }
}

/// A top-level module name: an environment slot (with whether it is a lexical declaration and whether it is const), or
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

impl Module {
    /// Compiles a module: gives the bytecode and the module info (requests, imports, exports, environment layout).
    pub fn compile_module(src: &Source<'_>) -> Result<(Self, ModuleInfo), Error> {
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
        let (names, info) = Self::module_layout(program);
        let order: Vec<&str> = names.iter().map(|&(n, _)| n).collect();
        let (captured, used, eval_fns) = Resolver::script(&program.body, Decls::this_key(program.span), &allocator, None, true, eval_scopes, Some(&[order]));
        let mut c = Compiler { src, alloc: &allocator, captured, used, sites: 0, class_depth: 0, module: Vec::new(), frames: Vec::new(), levels: Vec::new(), script_lexicals: HashSet::new(), chains: Vec::new(), eval_fns };
        let bound = names
            .iter()
            .map(|&(name, n)| match n {
                ModuleName::Slot { slot, lexical, constant } => Bound { name, binding: Binding::Env(slot), constant, ready: lexical.then_some(u32::MAX), self_name: false },
                ModuleName::Import(i) => Bound { name, binding: Binding::Import(i), constant: true, ready: None, self_name: false },
            })
            .collect();
        c.levels.push(Level { func: usize::MAX, names: bound, env: true, mark: 0, switch: false });
        c.module_body(program)?;
        c.module.iter_mut().for_each(Function::fuse);
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
    pub(crate) fn file(&self) -> &'a str {
        self.alloc.alloc_str(self.src.file)
    }

    pub(crate) fn unsupported(&self, span: Span, what: &str) -> Error {
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
        match self.frame().var_level {
            Some(level) => {
                let mut depth = 0;
                for (i, l) in self.levels.iter().enumerate().rev() {
                    if i <= level
                        && let Some(n) = l.names.iter().find(|n| n.name == name)
                    {
                        let insn = match n.binding {
                            Binding::Reg(r) => Insn::Star(r),
                            Binding::Env(slot) => Insn::StaEnv { depth, slot },
                            Binding::Import(_) => Insn::ThrowConstAssign,
                        };
                        self.emit(insn);
                        return;
                    }
                    if l.env {
                        depth += 1;
                    }
                }
            }
            None => match self.frame().eval_vars {
                Some(vars) => {
                    let v = self.temp();
                    self.emit(Insn::Star(v));
                    let obj = self.temp();
                    self.load(vars);
                    self.emit(Insn::Star(obj));
                    self.emit(Insn::Ldar(v));
                    let k = self.name(name);
                    self.emit(Insn::SetNamed { obj, name: k, strict: false });
                    self.release(obj);
                    self.release(v);
                }
                None => {
                    let k = self.name(name);
                    self.emit(Insn::StaGlobal { name: k, strict: false });
                }
            },
        }
    }

    /// Conflict check of EvalDeclarationInstantiation: an eval `var` / function name equal to a lexical declaration
    /// between the call site and the variable environment.
    fn eval_conflicts(&self, globals: &Decls<'a>) -> Result<(), Error> {
        for d in globals.0.iter().filter(|d| matches!(d.kind, DeclKind::Var | DeclKind::Function(_))) {
            if self.outer_lexical(d.name) {
                return Err(Error::Syntax(format!("{}: Identifier '{}' has already been declared", self.src.file, d.name)));
            }
        }
        Ok(())
    }

    /// Whether there is a lexical declaration with this name between the direct eval and the variable environment
    /// (including the variable environment level).
    fn outer_lexical(&self, name: &str) -> bool {
        for l in self.levels.iter().rev().filter(|l| l.func == usize::MAX) {
            if l.names.iter().any(|n| n.name == name && n.ready.is_some() && !n.self_name) {
                return true;
            }
            if l.names.first().is_some_and(|n| Hidden::is_eval_vars(n.name)) {
                break;
            }
        }
        false
    }

    /// Whether the name resolves to the own name of a named function expression.
    fn is_self_name(&self, name: &str) -> bool {
        self.levels.iter().rev().find_map(|l| l.names.iter().find(|n| n.name == name)).is_some_and(|n| n.self_name)
    }

    /// Direct eval code: the calling function's level (hidden name of the object holding eval vars, names already at
    /// that level).
    fn eval_var_level(&self) -> Option<(&'a str, Vec<&'a str>)> {
        self.levels.iter().rev().filter(|l| l.func == usize::MAX).find_map(|l| {
            let first = l.names.first()?;
            Hidden::is_eval_vars(first.name).then(|| (first.name, l.names.iter().map(|n| n.name).collect()))
        })
    }

    /// Scope table at a direct eval: all bindings currently visible (all in environments), innermost first, one
    /// environment per level.
    pub(crate) fn eval_scope(&mut self) -> u16 {
        let mut levels = Vec::new();
        let cur = self.frames.len() - 1;
        let prologue = self.frame().prologue;
        for l in self.levels.iter().rev().filter(|l| l.env) {
            // In parameter expressions: the top-level let / const / class of the function body are not visible yet
            let hide_lexical = prologue && l.func == cur;
            let names = l
                .names
                .iter()
                .filter(|n| !(hide_lexical && n.ready.is_some()))
                .filter_map(|n| match n.binding {
                    Binding::Env(slot) => Some(EvalName { name: n.name.into(), slot, constant: n.constant, tdz: n.ready.is_some(), self_name: n.self_name }),
                    Binding::Reg(_) | Binding::Import(_) => None,
                })
                .collect();
            levels.push(names);
        }
        let scopes = &mut self.frame().out.scopes;
        scopes.push(EvalScope(levels));
        u16::try_from(scopes.len() - 1).unwrap_or(u16::MAX)
    }

    /// Whether the code here is strict: the function itself is strict, or it is inside a class body (`extends` and
    /// computed keys are evaluated in the outer function, also as strict code).
    pub(crate) fn strict_here(&self) -> bool {
        self.strict() || self.class_depth > 0
    }

    /// Whether the current function is in strict mode.
    pub(crate) fn strict(&self) -> bool {
        self.frames.last().is_some_and(|f| f.strict)
    }

    fn frame(&mut self) -> &mut Frame<'a> {
        self.frames.last_mut().expect("inside a function")
    }

    pub(crate) fn emit(&mut self, i: Insn) -> usize {
        let out = &mut self.frame().out;
        // Number constants exactly representable as i32 (excluding -0) go directly into the instruction
        let i = match i {
            Insn::LdaConst(k) => match out.consts.get(k.0 as usize) {
                #[expect(clippy::cast_possible_truncation, reason = "checked below that it round-trips")]
                Some(&Constant::Number(x)) if f64::from(x as i32) == x && !(x == 0.0 && x.is_sign_negative()) => Insn::LdaSmi(x as i32),
                _ => i,
            },
            _ => i,
        };
        out.insns.push(i);
        out.insns.len() - 1
    }

    pub(crate) fn here(&mut self) -> Label {
        Label(u32::try_from(self.frame().out.insns.len()).expect("fewer than 2^32 instructions"))
    }

    /// Emits a jump whose target is to be backpatched.
    pub(crate) fn jump(&mut self, mk: fn(Label) -> Insn) -> usize {
        self.emit(mk(UNPATCHED))
    }

    pub(crate) fn patch(&mut self, at: usize, to: Label) {
        *self.frame().out.insns[at].target_mut().expect("a jump") = to;
    }

    /// Points the jump at `at` here.
    pub(crate) fn land(&mut self, at: usize) {
        let to = self.here();
        self.patch(at, to);
    }

    pub(crate) fn constant(&mut self, c: Constant) -> Const {
        let consts = &mut self.frame().out.consts;
        let k = consts.iter().position(|x| *x == c).unwrap_or_else(|| {
            consts.push(c);
            consts.len() - 1
        });
        Const(u32::try_from(k).expect("fewer than 2^32 constants"))
    }

    pub(crate) fn name(&mut self, s: &str) -> Const {
        self.constant(Constant::String(s.into()))
    }

    /// `n` consecutive temporary registers, [`release`](Self::release)d in stack order.
    pub(crate) fn temps(&mut self, n: u16) -> Reg {
        let f = self.frame();
        let r = Reg(f.top);
        f.top += n;
        f.out.regs = f.out.regs.max(f.top);
        r
    }

    pub(crate) fn temp(&mut self) -> Reg {
        self.temps(1)
    }

    pub(crate) fn release(&mut self, r: Reg) {
        self.frame().top = r.0;
    }

    // ---- Scopes ------------------------------------------------------------------------

    /// Enters a scope level: allocates bindings (captured ones go into the environment), copies in the captured `this`
    /// / parameters, creates closures for function declarations.
    fn enter(&mut self, decls: &Decls<'a>) -> Result<(), Error> {
        let func = self.frames.len() - 1;
        let mark = self.frame().top;
        let mut slots = 0u16;
        let mut names = Vec::with_capacity(decls.0.len());
        for d in &decls.0 {
            let binding = if self.captured.contains(&d.key) {
                slots += 1;
                Binding::Env(slots - 1)
            } else {
                match d.kind {
                    DeclKind::This => Binding::Reg(Reg::THIS),
                    DeclKind::SelfName => Binding::Reg(Reg::CALLEE),
                    DeclKind::Param(i) => Binding::Reg(Reg(Reg::PARAMS + i)),
                    _ => Binding::Reg(self.temp()),
                }
            };
            names.push(Bound { name: d.name, binding, constant: matches!(d.kind, DeclKind::Const), ready: d.ready, self_name: matches!(d.kind, DeclKind::SelfName) });
        }
        let env = slots > 0;
        if env {
            self.emit(Insn::PushEnv(slots));
            self.frame().envs += 1;
        }
        let bindings: Vec<Binding> = names.iter().map(|n| n.binding).collect();
        self.levels.push(Level { func, names, env, mark, switch: false });
        // Temporal dead zone: slots are first filled with the uninitialized marker
        for (d, &b) in decls.0.iter().zip(&bindings) {
            if d.ready.is_some() {
                self.emit(Insn::LdaHole);
                self.bind(b);
            }
        }
        for (d, b) in decls.0.iter().zip(bindings) {
            let fixed = match d.kind {
                DeclKind::This => Some(Reg::THIS),
                DeclKind::SelfName => Some(Reg::CALLEE),
                DeclKind::Param(i) => Some(Reg(Reg::PARAMS + i)),
                DeclKind::Function(f) => {
                    let n = self.function_declaration(f)?;
                    self.emit(Insn::CreateClosure(n));
                    self.bind(b);
                    None
                }
                DeclKind::NewTarget if self.used.contains(&d.key) => {
                    self.emit(Insn::LdaNewTarget);
                    self.bind(b);
                    None
                }
                DeclKind::Arguments if self.used.contains(&d.key) => {
                    // Creating arguments needs all arguments; with sloppy mode and simple parameters, indices map to
                    // the parameter slots
                    self.frame().out.keeps_args = true;
                    if self.frame().simple_params && !self.frame().strict {
                        let level = self.levels.last().expect("the function level");
                        let mut slots = alloc_vec_slots(decls, level);
                        slots.truncate(usize::from(self.frame().out.params));
                        self.frame().out.mapped_params = slots;
                    }
                    self.emit(Insn::CreateArguments);
                    self.bind(b);
                    None
                }
                DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::NewTarget | DeclKind::Arguments => None,
            };
            if let (Some(r), Binding::Env(_)) = (fixed, b) {
                self.emit(Insn::Ldar(r));
                self.bind(b);
            }
        }
        Ok(())
    }

    /// Stores acc into the binding just allocated at this level.
    fn bind(&mut self, b: Binding) {
        match b {
            Binding::Reg(r) => self.emit(Insn::Star(r)),
            Binding::Env(slot) => self.emit(Insn::StaEnv { depth: 0, slot }),
            Binding::Import(_) => self.emit(Insn::ThrowConstAssign),
        };
    }

    /// Leaves a block scope level.
    fn leave(&mut self) {
        let l = self.levels.pop().expect("a scope to leave");
        if l.env {
            self.emit(Insn::PopEnv);
            self.frame().envs -= 1;
        }
        self.frame().top = l.mark;
    }

    fn scoped(&mut self, decls: &Decls<'a>, f: impl FnOnce(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        self.enter(decls)?;
        f(self)?;
        self.leave();
        Ok(())
    }

    /// Name resolution: innermost first, counting the environment levels passed; global if not found.
    pub(crate) fn resolve(&self, name: &'a str) -> Resolved<'a> {
        let cur = self.frames.len() - 1;
        let mut depth = 0;
        for l in self.levels.iter().rev() {
            if let Some(n) = l.names.iter().find(|n| n.name == name) {
                let (place, in_env) = match n.binding {
                    Binding::Reg(r) => {
                        assert_eq!(l.func, cur, "an outer function's binding `{name}` used from inside must be captured");
                        (Place::Reg(r), false)
                    }
                    Binding::Env(slot) => (Place::Env { depth, slot }, true),
                    Binding::Import(i) => (Place::Import(i), false),
                };
                return Resolved { place, constant: n.constant, tdz: n.ready.map(|r| (r, in_env || l.switch)) };
            }
            if l.env {
                depth += 1;
            }
        }
        Resolved { place: Place::Global(name), constant: false, tdz: None }
    }

    /// If the name is in this function's registers, needs no TDZ check at `pos`, and is not inside `with`, gives that
    /// register. Only this function's own code can change it (bindings captured by inner functions are in environments,
    /// not registers).
    pub(crate) fn register_of(&self, name: &'a str, pos: u32) -> Option<Reg> {
        if !self.with_chain(name).is_empty() {
            return None;
        }
        let r = self.resolve(name);
        match r.place {
            Place::Reg(reg) if !r.needs_check(pos) => Some(reg),
            _ => None,
        }
    }

    pub(crate) fn lookup(&self, name: &'a str) -> (Place<'a>, bool) {
        let r = self.resolve(name);
        (r.place, r.constant)
    }

    /// Reads a name into acc (use position unknown, so the TDZ is checked).
    pub(crate) fn load(&mut self, name: &'a str) {
        self.load_at(name, 0);
    }

    /// The `with`s between a name and its static binding (innermost first, giving the hidden name of each object).
    pub(crate) fn with_chain(&self, name: &'a str) -> Vec<&'a str> {
        let mut out = Vec::new();
        for l in self.levels.iter().rev() {
            if l.names.iter().any(|n| n.name == name) {
                break;
            }
            if let Some(n) = l.names.first()
                && Hidden::is_with(n.name)
            {
                out.push(n.name);
            }
        }
        out
    }

    /// Checks the `with` objects in turn: if one has the name, reads it (putting the object into `this_reg`, if given,
    /// as the call's `this`) and jumps to the end. Gives the jumps to patch to the end.
    fn with_reads(&mut self, name: &'a str, chain: &[&'a str], this_reg: Option<Reg>) -> Vec<usize> {
        let k = self.name(name);
        let obj = self.temp();
        let mut ends = Vec::new();
        for &hidden in chain {
            self.load(hidden);
            self.emit(Insn::Star(obj));
            self.emit(Insn::WithHas { obj, name: k });
            let skip = self.jump(Insn::JumpIfFalse);
            // The object holding eval vars acts as a declarative environment: it is not the call's `this`
            if let Some(t) = this_reg.filter(|_| !Hidden::is_eval_vars(hidden)) {
                self.emit(Insn::Ldar(obj));
                self.emit(Insn::Star(t));
            }
            self.emit(Insn::GetNamed { obj, name: k });
            ends.push(self.jump(Insn::Jump));
            self.land(skip);
        }
        self.release(obj);
        ends
    }

    /// Reads a name, checking the object first inside `with`; for `typeof` gives `undefined` when not found; for calls
    /// puts the object into `this_reg`.
    pub(crate) fn load_dynamic(&mut self, name: &'a str, typeof_safe: bool, this_reg: Option<Reg>) {
        let chain = self.with_chain(name);
        let ends = self.with_reads(name, &chain, this_reg);
        match self.resolve(name).place {
            Place::Global(g) if typeof_safe && !matches!(g, "undefined" | "NaN" | "Infinity") => {
                let k = self.name(g);
                self.emit(Insn::LdaGlobalOrUndefined(k));
            }
            _ => self.load_static(name, 0),
        }
        for e in ends {
            self.land(e);
        }
    }

    /// Reads a name into acc at source position `pos`. `undefined` / `NaN` / `Infinity` are constants unless shadowed.
    pub(crate) fn load_at(&mut self, name: &'a str, pos: u32) {
        if !self.with_chain(name).is_empty() {
            return self.load_dynamic(name, false, None);
        }
        self.load_static(name, pos);
    }

    fn load_static(&mut self, name: &'a str, pos: u32) {
        let r = self.resolve(name);
        let insn = match r.place {
            Place::Reg(reg) => Insn::Ldar(reg),
            Place::Env { depth, slot } => Insn::LdaEnv { depth, slot },
            Place::Global("undefined") => Insn::LdaUndefined,
            Place::Global("NaN") => Insn::LdaConst(self.constant(Constant::Number(f64::NAN))),
            Place::Global("Infinity") => Insn::LdaConst(self.constant(Constant::Number(f64::INFINITY))),
            Place::Global(g) => Insn::LdaGlobal(self.name(g)),
            Place::Import(index) => Insn::LdaImport { index },
        };
        self.emit(insn);
        if r.needs_check(pos) {
            let check = if name == THIS { Insn::CheckThis } else { Insn::CheckInit { name: self.name(name) } };
            self.emit(check);
        }
    }

    /// Stores acc into a name; `init` is the initialization in a declaration (the only time `const` allows it, with no
    /// TDZ check).
    pub(crate) fn store(&mut self, name: &'a str, init: bool) {
        self.store_at(name, init, 0);
    }

    /// Same as [`store`](Self::store); `pos` is the source position of the assignment: no TDZ check after the
    /// declaration.
    pub(crate) fn store_at(&mut self, name: &'a str, init: bool, pos: u32) {
        let chain = self.with_chain(name);
        if chain.is_empty() {
            return self.store_static(name, init, pos);
        }
        // Inside `with`: if the object has this name write to it, otherwise write the static binding
        let value = self.temp();
        self.emit(Insn::Star(value));
        let k = self.name(name);
        let obj = self.temp();
        let strict = self.strict_here();
        let mut ends = Vec::new();
        for hidden in chain {
            self.load(hidden);
            self.emit(Insn::Star(obj));
            self.emit(Insn::WithHas { obj, name: k });
            let skip = self.jump(Insn::JumpIfFalse);
            self.emit(Insn::Ldar(value));
            self.emit(Insn::SetNamed { obj, name: k, strict });
            ends.push(self.jump(Insn::Jump));
            self.land(skip);
        }
        self.emit(Insn::Ldar(value));
        self.store_static(name, init, pos);
        for e in ends {
            self.land(e);
        }
        self.emit(Insn::Ldar(value));
        self.release(obj);
        self.release(value);
    }

    fn store_static(&mut self, name: &'a str, init: bool, pos: u32) {
        if !init && self.is_self_name(name) {
            if self.strict_here() {
                self.emit(Insn::ThrowConstAssign);
            }
            return;
        }
        let r = self.resolve(name);
        if r.constant && !init {
            self.emit(Insn::ThrowConstAssign);
            return;
        }
        if !init && r.needs_check(pos) {
            let k = self.name(name);
            match r.place {
                Place::Reg(reg) => _ = self.emit(Insn::CheckReg { reg, name: k }),
                Place::Env { depth, slot } => _ = self.emit(Insn::CheckEnv { depth, slot, name: k }),
                Place::Global(_) | Place::Import(_) => {}
            }
        }
        let insn = match r.place {
            Place::Reg(reg) => Insn::Star(reg),
            Place::Env { depth, slot } => Insn::StaEnv { depth, slot },
            Place::Global(g) if init && self.script_lexicals.contains(g) => Insn::InitLexical { name: self.name(g) },
            Place::Global(g) => Insn::StaGlobal { name: self.name(g), strict: self.strict_here() },
            Place::Import(_) => Insn::ThrowConstAssign,
        };
        self.emit(insn);
    }

    // ---- Functions ---------------------------------------------------------------------

    /// Compiles a function, returning its index in the module.
    pub(crate) fn function(&mut self, name: &str, flavor: Flavor, this: Option<Key>, self_name: Option<&'a BindingIdentifier<'a>>, params: Option<&'a FormalParameters<'a>>, body: Body<'a>) -> Result<u32, Error> {
        let count = params.map_or(0, |p| p.items.len());
        let n = u16::try_from(count).map_err(|_| self.unsupported(params.map_or(Span::default(), |p| p.span), "more than 65535 parameters"))?;
        let index = self.module.len();
        self.module.push(Function::default());
        let top = Reg::PARAMS + n;
        let keeps_args = matches!(body, Body::DefaultConstructor { derived: true }) || params.is_some_and(|p| p.rest.is_some());
        // length: the number of parameters before the first one with a default value (the rest parameter does not
        // count)
        let length = u16::try_from(params.map_or(0, |p| p.items.iter().take_while(|i| i.initializer.is_none()).count())).unwrap_or(u16::MAX);
        // Functions in a class (methods, constructors, field initializers) are all strict
        let strict = flavor.strict || self.class_depth > 0 || self.frames.last().is_some_and(|f| f.strict);
        let out = Function { name: name.into(), kind: flavor.kind, keeps_args, generator: flavor.generator, asynchronous: flavor.asynchronous, mode: strict.into(), params: n, length, regs: top, ..Function::default() };
        self.frames.push(Frame { index, out, top, envs: 0, controls: Vec::new(), label: None, completion: None, resume: None, async_generator: false, strict, annexb: Vec::new(), var_level: None, eval_vars: None, prologue: false, simple_params: false });
        if flavor.generator || flavor.asynchronous {
            let mode = self.temp();
            self.frame().resume = Some(mode);
            self.frame().async_generator = flavor.generator && flavor.asynchronous;
        }
        let stmts = match body {
            Body::Statements(s) => s,
            _ => &[],
        };
        self.frame().simple_params = params.is_some_and(|p| p.rest.is_none() && p.items.iter().all(|i| i.initializer.is_none() && matches!(i.pattern, BindingPattern::BindingIdentifier(_))));
        let eval_vars = params.filter(|p| !strict && self.eval_fns.contains(&p.span.start)).map(|p| Hidden::eval_vars(self.alloc, p));
        self.enter(&Decls::function(this, self_name, params, stmts, !strict, eval_vars))?;
        if let Some(h) = eval_vars {
            self.emit(Insn::CreateNullObject);
            self.store(h, true);
        }
        if !strict {
            self.frame().var_level = Some(self.levels.len() - 1);
            self.frame().annexb = Decls::annexb_functions(stmts, params).iter().map(|f| f.span.start).collect();
        }
        if flavor.kind == (FunctionKind::Constructor { derived: true })
            && let Some(t) = self.levels.last_mut().and_then(|l| l.names.iter_mut().find(|n| n.name == THIS))
        {
            // In a derived class constructor `this` is uninitialized before `super()`
            t.ready = Some(u32::MAX);
        }
        if let Some(p) = params {
            self.frame().prologue = true;
            self.prologue(p)?;
            self.frame().prologue = false;
        }
        // Generator: suspend only after parameters and declarations are set up (a call runs up to here, so parameter
        // errors throw synchronously); the first next resumes from here
        if flavor.generator
            && let Some(mode) = self.frame().resume
        {
            self.emit(Insn::LdaUndefined);
            self.emit(Insn::Yield { mode });
            self.resume_dispatch(mode, true)?;
        }
        match body {
            Body::Statements(s) => self.statements(s)?,
            Body::Expression(e) => {
                self.expr(e)?;
                self.emit(Insn::Return);
            }
            Body::Fields { class, statik } => self.fields(class, statik)?,
            Body::DefaultConstructor { derived } => {
                if derived {
                    self.emit(Insn::SuperCallForward);
                }
            }
        }
        self.emit(Insn::LdaUndefined);
        self.emit(Insn::Return);
        self.levels.pop();
        let f = self.frames.pop().expect("the function's frame");
        self.module[f.index] = f.out;
        Ok(u32::try_from(index).expect("fewer than 2^32 functions"))
    }

    /// Parameter prologue: default values, destructured parameters, rest parameter.
    fn prologue(&mut self, params: &'a FormalParameters<'a>) -> Result<(), Error> {
        for (i, p) in (0u16..).zip(&params.items) {
            let reg = Reg(Reg::PARAMS + i);
            if let Some(init) = &p.initializer {
                self.emit(Insn::Ldar(reg));
                let skip = self.jump(Insn::JumpIfNotUndefined);
                match &p.pattern {
                    BindingPattern::BindingIdentifier(id) => self.expr_named(init, id.name.as_str())?,
                    _ => self.expr(init)?,
                }
                self.emit(Insn::Star(reg));
                self.land(skip);
            }
            match &p.pattern {
                BindingPattern::BindingIdentifier(id) => {
                    // Captured parameters were copied into the environment on frame entry; after the default value is
                    // computed they must be copied again
                    let name = id.name.as_str();
                    if p.initializer.is_some() && !matches!(self.lookup(name).0, Place::Reg(r) if r == reg) {
                        self.emit(Insn::Ldar(reg));
                        self.store(name, true);
                    }
                }
                pat => self.bind_pattern(pat, reg, true)?,
            }
        }
        if let Some(r) = &params.rest {
            let from = u16::try_from(params.items.len()).map_err(|_| self.unsupported(r.span, "more than 65535 parameters"))?;
            self.emit(Insn::CreateRest(from));
            let t = self.temp();
            self.emit(Insn::Star(t));
            self.bind_pattern(&r.rest.argument, t, true)?;
            self.release(t);
        }
        Ok(())
    }

    /// Binds the value in register `value` to names according to the pattern (`init`: initialization in a declaration).
    pub(crate) fn bind_pattern(&mut self, p: &'a BindingPattern<'a>, value: Reg, init: bool) -> Result<(), Error> {
        match p {
            BindingPattern::BindingIdentifier(id) => {
                self.emit(Insn::Ldar(value));
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
                let n = u16::try_from(o.properties.len()).map_err(|_| self.unsupported(o.span, "more than 65535 properties"))?;
                let keys = self.temps(n);
                for (i, b) in (0u16..).zip(&o.properties) {
                    let k = Reg(keys.0 + i);
                    self.property_key_into(&b.key, b.computed, k)?;
                    self.emit(Insn::Ldar(k));
                    self.emit(Insn::GetKeyed { obj: value });
                    let t = self.temp();
                    self.emit(Insn::Star(t));
                    self.bind_pattern(&b.value, t, init)?;
                    self.release(t);
                }
                if let Some(r) = &o.rest {
                    self.emit(Insn::ObjectRest { src: value, keys, count: n });
                    let t = self.temp();
                    self.emit(Insn::Star(t));
                    self.bind_pattern(&r.argument, t, init)?;
                    self.release(t);
                } else if o.properties.is_empty() {
                    self.require_object(value);
                }
                self.release(keys);
            }
            BindingPattern::ArrayPattern(a) => {
                let iter = self.iterate(value);
                // If a default value throws, the iterator must be closed first
                self.closing_on_throw(iter, |c| {
                    for e in &a.elements {
                        c.emit(Insn::IteratorNext { iter });
                        let Some(e) = e else { continue };
                        let t = c.temp();
                        c.emit(Insn::Star(t));
                        c.bind_pattern(e, t, init)?;
                        c.release(t);
                    }
                    match &a.rest {
                        Some(r) => {
                            c.emit(Insn::IteratorRest { iter });
                            let t = c.temp();
                            c.emit(Insn::Star(t));
                            c.bind_pattern(&r.argument, t, init)?;
                            c.release(t);
                        }
                        None => _ = c.emit(Insn::IteratorClose { iter }),
                    }
                    Ok(())
                })?;
                self.release(iter);
            }
        }
        Ok(())
    }

    /// Runs `f`; if it throws, closes iterator `iter` first (calling `return` only if not done) and then rethrows.
    pub(crate) fn closing_on_throw(&mut self, iter: Reg, f: impl FnOnce(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        let on_throw = self.jump(Insn::PushHandler);
        self.frame().controls.push(Control::Handler);
        f(self)?;
        self.frame().controls.pop();
        self.emit(Insn::PopHandler);
        let end = self.jump(Insn::Jump);
        self.land(on_throw);
        let e = self.temp();
        self.emit(Insn::Star(e));
        self.emit(Insn::IteratorCloseQuiet { iter });
        self.emit(Insn::Ldar(e));
        self.emit(Insn::Throw);
        self.release(e);
        self.land(end);
        Ok(())
    }

    /// Gets the iterator of register `value`: the iterator record takes three consecutive registers (the caller frees
    /// them).
    pub(crate) fn iterate(&mut self, value: Reg) -> Reg {
        self.emit(Insn::Ldar(value));
        let iter = self.temps(3);
        self.emit(Insn::GetIterator { dst: iter });
        iter
    }

    /// The left side of `for-of` / `for-in`: a declaration (including destructuring) or an assignment target, with the
    /// value in `v`.
    fn for_left(&mut self, left: &'a ForStatementLeft<'a>, v: Reg) -> Result<(), Error> {
        match left {
            ForStatementLeft::VariableDeclaration(d) => {
                let decl = d.declarations.first().ok_or_else(|| self.unsupported(d.span, "an empty declaration"))?;
                self.bind_pattern(&decl.id, v, true)
            }
            t => self.assign_target(t.as_assignment_target().ok_or_else(|| self.unsupported(t.span(), "for-in / for-of target"))?, v),
        }
    }

    /// `for (left of right) body`: iterator protocol; `break` / `return` / a throw all close the iterator.
    /// `for await (left of right) body`: async iterator (falling back to the sync iterator and awaiting each value when
    /// absent), awaiting the result of `next()` at each step; `break` / `return` passing out call `return` and await
    /// it, a throw from the body closes the iterator.
    fn for_await(&mut self, f: &'a ForOfStatement<'a>) -> Result<(), Error> {
        let mode = self.resume_reg(f.span, "for await outside an async function")?;
        let label = self.take_label();
        self.expr(&f.right)?;
        let iter = self.temps(4);
        self.emit(Insn::GetAsyncIterator { dst: iter });
        self.frame().controls.push(Control::AsyncIterator(iter));
        let v = self.temp();
        let top = self.here();
        self.emit(Insn::IteratorCall { iter });
        self.emit(Insn::Await { mode });
        self.resume_dispatch(mode, false)?;
        self.emit(Insn::IteratorResult { iter });
        self.emit(Insn::Star(v));
        self.emit(Insn::Ldar(Reg(iter.0 + 2)));
        let done = self.jump(Insn::JumpIfTrue);
        // When falling back to the sync iterator the value itself is awaited too
        self.emit(Insn::Ldar(Reg(iter.0 + 3)));
        let not_sync = self.jump(Insn::JumpIfFalse);
        self.emit(Insn::Ldar(v));
        self.emit(Insn::Await { mode });
        self.resume_dispatch(mode, false)?;
        self.emit(Insn::Star(v));
        self.land(not_sync);
        self.push_jump(label, true, true);
        let on_throw = self.jump(Insn::PushHandler);
        self.frame().controls.push(Control::Handler);
        self.scoped(&Decls::for_head(&f.left), |c| {
            c.for_left(&f.left, v)?;
            c.statement(&f.body)
        })?;
        self.frame().controls.pop();
        self.emit(Insn::PopHandler);
        self.emit(Insn::Jump(top));
        // break lands here: call return and await it
        let broke = self.here();
        self.emit(Insn::AsyncIteratorReturn { iter });
        self.emit(Insn::Await { mode });
        self.resume_dispatch(mode, false)?;
        let end = self.jump(Insn::Jump);
        self.land(on_throw);
        self.emit(Insn::Star(v));
        self.emit(Insn::IteratorCloseQuiet { iter });
        self.emit(Insn::Ldar(v));
        self.emit(Insn::Throw);
        self.pop_jump(broke, Some(top));
        self.land(done);
        self.land(end);
        self.frame().controls.pop();
        self.release(iter);
        Ok(())
    }

    fn for_of(&mut self, f: &'a ForOfStatement<'a>) -> Result<(), Error> {
        if f.r#await {
            return self.for_await(f);
        }
        let label = self.take_label();
        self.expr(&f.right)?;
        let src = self.temp();
        self.emit(Insn::Star(src));
        let iter = self.iterate(src);
        self.frame().controls.push(Control::Iterator(iter));
        let v = self.temp();
        let top = self.here();
        self.emit(Insn::IteratorNext { iter });
        self.emit(Insn::Star(v));
        self.emit(Insn::Ldar(Reg(iter.0 + 2)));
        let done = self.jump(Insn::JumpIfTrue);
        self.push_jump(label, true, true);
        let on_throw = self.jump(Insn::PushHandler);
        self.frame().controls.push(Control::Handler);
        self.scoped(&Decls::for_head(&f.left), |c| {
            c.for_left(&f.left, v)?;
            c.statement(&f.body)
        })?;
        self.frame().controls.pop();
        self.emit(Insn::PopHandler);
        self.emit(Insn::Jump(top));
        // break lands here: close the iterator
        let broke = self.here();
        self.emit(Insn::IteratorClose { iter });
        let end = self.jump(Insn::Jump);
        // Throw from the body: close the iterator and rethrow
        self.land(on_throw);
        self.emit(Insn::Star(v));
        self.emit(Insn::IteratorCloseQuiet { iter });
        self.emit(Insn::Ldar(v));
        self.emit(Insn::Throw);
        self.pop_jump(broke, Some(top));
        self.land(done);
        self.land(end);
        self.frame().controls.pop();
        self.release(src);
        Ok(())
    }

    /// `for (left in right) body`: takes a snapshot of the key list first and walks it by index.
    fn for_in(&mut self, f: &'a ForInStatement<'a>) -> Result<(), Error> {
        let label = self.take_label();
        self.expr(&f.right)?;
        self.emit(Insn::ForInKeys);
        let keys = self.temp();
        self.emit(Insn::Star(keys));
        let i = self.temp();
        let zero = self.constant(Constant::Number(0.0));
        self.emit(Insn::LdaConst(zero));
        self.emit(Insn::Star(i));
        let v = self.temp();
        let top = self.here();
        let length = self.name("length");
        self.emit(Insn::GetNamed { obj: keys, name: length });
        self.emit(Insn::Compare(CompareOp::Lt, i));
        let exit = self.jump(Insn::JumpIfFalse);
        self.emit(Insn::Ldar(i));
        self.emit(Insn::GetKeyed { obj: keys });
        self.emit(Insn::Star(v));
        self.push_jump(label, true, true);
        self.scoped(&Decls::for_head(&f.left), |c| {
            c.for_left(&f.left, v)?;
            c.statement(&f.body)
        })?;
        let cont = self.here();
        self.emit(Insn::Ldar(i));
        self.emit(Insn::Inc);
        self.emit(Insn::Star(i));
        self.emit(Insn::Jump(top));
        self.land(exit);
        let end = self.here();
        self.pop_jump(end, Some(cont));
        self.release(keys);
        Ok(())
    }

    /// After `yield` / `await`: dispatches on the resume mode (`return` goes through finally, `throw` throws here),
    /// otherwise acc is the value sent in.
    pub(crate) fn resume_dispatch(&mut self, mode: Reg, returnable: bool) -> Result<(), Error> {
        let v = self.temp();
        self.emit(Insn::Star(v));
        if returnable {
            self.on_completion(mode, f64::from(Resume::Return), |s| {
                s.emit(Insn::Ldar(v));
                if s.frame().async_generator {
                    // return(v) of an async generator awaits v first
                    s.emit(Insn::Await { mode });
                    s.resume_dispatch(mode, false)?;
                }
                s.ret();
                Ok(())
            })?;
        }
        self.on_completion(mode, f64::from(Resume::Throw), |s| {
            s.emit(Insn::Ldar(v));
            s.emit(Insn::Throw);
            Ok(())
        })?;
        self.emit(Insn::Ldar(v));
        self.release(v);
        Ok(())
    }

    pub(crate) fn is_async_generator(&mut self) -> bool {
        self.frame().async_generator
    }

    /// The register holding a generator / async function's resume mode; an error if not inside one.
    pub(crate) fn resume_reg(&mut self, span: Span, what: &str) -> Result<Reg, Error> {
        self.frame().resume.ok_or_else(|| self.unsupported(span, what))
    }

    /// Replaces `value` with the default value if it is `undefined`: the result goes into a new temporary register (the
    /// caller frees it).
    /// `name` is the name inferred for the default value when it is an anonymous function.
    pub(crate) fn with_default(&mut self, value: Reg, default: &'a Expression<'a>, name: Option<&'a str>) -> Result<Reg, Error> {
        let t = self.temp();
        self.emit(Insn::Ldar(value));
        self.emit(Insn::Star(t));
        let skip = self.jump(Insn::JumpIfNotUndefined);
        match name {
            Some(n) => self.expr_named(default, n)?,
            None => self.expr(default)?,
        }
        self.emit(Insn::Star(t));
        self.land(skip);
        Ok(t)
    }

    /// Destructuring `null` / `undefined` throws TypeError (added for `{} = v`, where there is no property to read).
    pub(crate) fn require_object(&mut self, value: Reg) {
        let k = self.name("constructor");
        self.emit(Insn::GetNamed { obj: value, name: k });
    }

    /// Evaluates a property key into register `k` (a static key is a string constant).
    pub(crate) fn property_key_into(&mut self, key: &'a PropertyKey<'a>, computed: bool, k: Reg) -> Result<(), Error> {
        match self.static_key(key, computed) {
            Some(c) => {
                let c = c?;
                self.emit(Insn::LdaConst(c));
            }
            None => self.expr(key.as_expression().ok_or_else(|| self.unsupported(key.span(), "property key"))?)?,
        }
        self.emit(Insn::Star(k));
        Ok(())
    }

    /// Ordinary function (declaration or expression): rejects unsupported kinds.
    /// Ordinary function; an anonymous one uses the inferred name `inferred`.
    pub(crate) fn plain_function(&mut self, f: &'a AstFunction<'a>, self_name: bool, inferred: &str) -> Result<u32, Error> {
        let flavor = Flavor::from((f, FunctionKind::Normal));
        let body = f.body.as_ref().ok_or_else(|| self.unsupported(f.span, "a function without a body"))?;
        let name = f.id.as_ref().map_or(inferred, |id| id.name.as_str());
        self.function(name, flavor, Some(Decls::this_key(f.span)), if self_name { f.id.as_ref() } else { None }, Some(&f.params), Body::Statements(&body.statements))
    }

    /// Method (class method, object literal method, accessor): not `new`able.
    pub(crate) fn method(&mut self, name: &str, f: &'a AstFunction<'a>, kind: FunctionKind) -> Result<u32, Error> {
        let flavor = Flavor::from((f, kind));
        let body = f.body.as_ref().ok_or_else(|| self.unsupported(f.span, "a method without a body"))?;
        self.function(name, flavor, Some(Decls::this_key(f.span)), None, Some(&f.params), Body::Statements(&body.statements))
    }

    /// The body of a field initializer function: for instances, first install private methods / accessors, then define
    /// fields in order (public ones enumerable, private ones into the private table); for statics, also run static
    /// blocks in order.
    fn fields(&mut self, class: &'a Class<'a>, statik: bool) -> Result<(), Error> {
        self.load(THIS);
        let this = self.temp();
        self.emit(Insn::Star(this));
        let key = self.temp();
        if !statik {
            for e in &class.body.body {
                if let ClassElement::MethodDefinition(m) = e
                    && !m.r#static
                    && let PropertyKey::PrivateIdentifier(id) = &m.key
                {
                    self.load(Hidden::private(self.alloc, id));
                    self.emit(Insn::Star(key));
                    self.load(Hidden::method(self.alloc, id, m.kind));
                    self.emit(Insn::DefinePrivate { obj: this, key, kind: Self::method_kind(m.kind), writable: false });
                }
            }
        }
        for e in &class.body.body {
            match e {
                ClassElement::PropertyDefinition(p) if p.r#static == statik => {
                    let fname = self.static_name(&p.key, p.computed);
                    let value = |c: &mut Self| match &p.value {
                        Some(v) => match fname {
                            Some(n) => c.expr_named(v, n),
                            None => c.expr(v),
                        },
                        None => {
                            c.emit(Insn::LdaUndefined);
                            Ok(())
                        }
                    };
                    match &p.key {
                        PropertyKey::PrivateIdentifier(id) => {
                            self.load(Hidden::private(self.alloc, id));
                            self.emit(Insn::Star(key));
                            value(self)?;
                            self.emit(Insn::DefinePrivate { obj: this, key, kind: PropertyKind::Data, writable: true });
                        }
                        _ if p.computed => {
                            self.load(Hidden::field_key(self.alloc, p.span));
                            self.emit(Insn::Star(key));
                            value(self)?;
                            self.emit(Insn::DefineKeyed { obj: this, key, kind: PropertyKind::Data, enumerable: true });
                        }
                        k => {
                            let name = self.static_key(k, false).ok_or_else(|| self.unsupported(p.span, "field name"))??;
                            value(self)?;
                            self.emit(Insn::DefineNamed { obj: this, name, kind: PropertyKind::Data, enumerable: true });
                        }
                    }
                }
                ClassElement::StaticBlock(b) if statik => self.scoped(&Decls::static_block(&b.body), |c| c.statements(&b.body))?,
                _ => {}
            }
        }
        self.release(this);
        Ok(())
    }

    /// Method definition kind ⇒ property kind.
    fn method_kind(kind: MethodDefinitionKind) -> PropertyKind {
        match kind {
            MethodDefinitionKind::Get => PropertyKind::Getter,
            MethodDefinitionKind::Set => PropertyKind::Setter,
            MethodDefinitionKind::Method | MethodDefinitionKind::Constructor => PropertyKind::Data,
        }
    }

    /// The text of a non-computed property key (for function name inference; private names include `#`); `None` for
    /// computed keys.
    pub(crate) fn static_name(&self, key: &'a PropertyKey<'a>, computed: bool) -> Option<&'a str> {
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
    pub(crate) fn method_name(&self, key: &'a PropertyKey<'a>, computed: bool, kind: MethodDefinitionKind) -> &'a str {
        let Some(n) = self.static_name(key, computed) else { return "" };
        match kind {
            MethodDefinitionKind::Get => self.alloc.alloc_str(&format!("get {n}")),
            MethodDefinitionKind::Set => self.alloc.alloc_str(&format!("set {n}")),
            MethodDefinitionKind::Method | MethodDefinitionKind::Constructor => n,
        }
    }

    /// The constant for a non-computed property key (identifier / string / number); `None` for computed keys.
    pub(crate) fn static_key(&mut self, key: &'a PropertyKey<'a>, computed: bool) -> Option<Result<Const, Error>> {
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

    /// Class: constructor + prototype methods + static methods + field initializer functions. The result (the
    /// constructor) is in acc.
    /// Each private name in the class gets a new private key (shared by getter / setter), stored in a binding of the
    /// class scope.
    fn private_names(&mut self, c: &'a Class<'a>) {
        let mut seen: Vec<&'a str> = Vec::new();
        for e in &c.body.body {
            let id = match e {
                ClassElement::MethodDefinition(m) => match &m.key {
                    PropertyKey::PrivateIdentifier(id) => id,
                    _ => continue,
                },
                ClassElement::PropertyDefinition(p) => match &p.key {
                    PropertyKey::PrivateIdentifier(id) => id,
                    _ => continue,
                },
                _ => continue,
            };
            let binding = Hidden::private(self.alloc, id);
            if seen.contains(&binding) {
                continue;
            }
            seen.push(binding);
            let desc = self.name(binding);
            self.emit(Insn::CreatePrivateName { desc });
            self.store(binding, true);
        }
    }

    /// Private methods / accessors: the function is created only once and stored in a hidden binding; static ones are
    /// installed on the constructor (`target`) now, instance ones at construction.
    fn private_method(&mut self, m: &'a MethodDefinition<'a>, id: &'a PrivateIdentifier<'a>, target: Reg) -> Result<(), Error> {
        let fname = self.method_name(&m.key, false, m.kind);
        let f = self.method(fname, &m.value, FunctionKind::Method)?;
        self.emit(Insn::CreateMethod { func: f, home: target });
        self.store(Hidden::method(self.alloc, id, m.kind), true);
        if m.r#static {
            let key = self.temp();
            self.load(Hidden::private(self.alloc, id));
            self.emit(Insn::Star(key));
            self.load(Hidden::method(self.alloc, id, m.kind));
            self.emit(Insn::DefinePrivate { obj: target, key, kind: Self::method_kind(m.kind), writable: false });
            self.release(key);
        }
        Ok(())
    }

    /// Class; an anonymous constructor uses the inferred name `inferred`.
    pub(crate) fn class(&mut self, c: &'a Class<'a>, inferred: &'a str) -> Result<(), Error> {
        // The class body (including extends) is entirely strict code
        self.class_depth += 1;
        let r = self.class_body(c, inferred);
        self.class_depth -= 1;
        r
    }

    fn class_body(&mut self, c: &'a Class<'a>, inferred: &'a str) -> Result<(), Error> {
        let name = c.id.as_ref().map_or(inferred, |id| id.name.as_str());
        let parent = match &c.heritage {
            Some(h) => {
                self.expr(&h.expression)?;
                let p = self.temp();
                self.emit(Insn::Star(p));
                Some(p)
            }
            None => None,
        };
        self.enter(&Decls::class_scope(c, self.alloc))?;
        self.private_names(c);
        let derived = parent.is_some();
        let ctor_def = c.body.body.iter().find_map(|e| match e {
            ClassElement::MethodDefinition(m) if m.kind == MethodDefinitionKind::Constructor => Some(m),
            _ => None,
        });
        let kind = FunctionKind::Constructor { derived };
        let func = match ctor_def {
            Some(m) => self.method(name, &m.value, kind)?,
            None => self.function(name, kind.into(), None, None, None, Body::DefaultConstructor { derived })?,
        };
        let proto = self.temp();
        self.emit(Insn::CreateClass { func, parent, proto });
        let ctor = self.temp();
        self.emit(Insn::Star(ctor));
        // The class name inside the class body: initialized as soon as the constructor is created (usable in static
        // blocks and static fields)
        if c.id.is_some() {
            self.store(name, true);
        }
        let (mut instance_fields, mut static_fields) = (false, false);
        for e in &c.body.body {
            match e {
                ClassElement::MethodDefinition(m) if m.kind != MethodDefinitionKind::Constructor => {
                    let target = if m.r#static { ctor } else { proto };
                    let kind = Self::method_kind(m.kind);
                    if let PropertyKey::PrivateIdentifier(id) = &m.key {
                        self.private_method(m, id, target)?;
                        if m.r#static { static_fields = true } else { instance_fields = true }
                        continue;
                    }
                    match self.static_key(&m.key, m.computed) {
                        Some(name) => {
                            let name = name?;
                            let fname = self.method_name(&m.key, m.computed, m.kind);
                            let f = self.method(fname, &m.value, FunctionKind::Method)?;
                            self.emit(Insn::CreateMethod { func: f, home: target });
                            self.emit(Insn::DefineNamed { obj: target, name, kind, enumerable: false });
                        }
                        None => {
                            let k = m.key.as_expression().ok_or_else(|| self.unsupported(m.span, "method key"))?;
                            self.expr(k)?;
                            let key = self.temp();
                            self.emit(Insn::Star(key));
                            let f = self.method("", &m.value, FunctionKind::Method)?;
                            self.emit(Insn::CreateMethod { func: f, home: target });
                            self.emit(Insn::DefineKeyed { obj: target, key, kind, enumerable: false });
                            self.release(key);
                        }
                    }
                }
                ClassElement::MethodDefinition(_) => {}
                ClassElement::PropertyDefinition(p) => {
                    if p.computed && !matches!(p.key, PropertyKey::PrivateIdentifier(_)) {
                        // Computed field names are evaluated once at class definition
                        let k = p.key.as_expression().ok_or_else(|| self.unsupported(p.span, "field key"))?;
                        self.expr(k)?;
                        self.emit(Insn::ToPropertyKey);
                        self.store(Hidden::field_key(self.alloc, p.span), true);
                    }
                    if p.r#static { static_fields = true } else { instance_fields = true }
                }
                ClassElement::StaticBlock(_) => static_fields = true,
                e => return Err(self.unsupported(e.span(), "class element")),
            }
        }
        if instance_fields {
            let f = self.function("", FunctionKind::Method.into(), Some(Decls::this_key(c.body.span)), None, None, Body::Fields { class: c, statik: false })?;
            self.emit(Insn::CreateMethod { func: f, home: proto });
            self.emit(Insn::SetClassFields { ctor });
        }
        if static_fields {
            let f = self.function("", FunctionKind::Method.into(), Some(Decls::this_key(c.span)), None, None, Body::Fields { class: c, statik: true })?;
            self.emit(Insn::CreateMethod { func: f, home: ctor });
            let callee = self.temps(2);
            self.emit(Insn::Star(callee));
            self.emit(Insn::Ldar(ctor));
            self.emit(Insn::Star(Reg(callee.0 + 1)));
            self.emit(Insn::Call { callee, this: Reg(callee.0 + 1), args: Reg(callee.0 + 2), argc: 0 });
            self.release(callee);
        }
        self.emit(Insn::Ldar(ctor));
        self.leave();
        if let Some(p) = parent {
            self.release(p);
        }
        Ok(())
    }

    fn function_declaration(&mut self, f: &'a AstFunction<'a>) -> Result<u32, Error> {
        self.plain_function(f, false, "")
    }

    /// Module body: top-level names all live in the module environment (the outer level); create the hoisted functions
    /// first, then run the statements in order.
    fn module_body(&mut self, program: &'a Program<'a>) -> Result<(), Error> {
        use oxc_ast::ast::{Declaration, ExportDefaultDeclarationKind};
        self.module.push(Function::default());
        let out = Function { name: "main".into(), regs: Reg::PARAMS, mode: true.into(), ..Function::default() };
        self.frames.push(Frame { index: 0, out, top: Reg::PARAMS, envs: 0, controls: Vec::new(), label: None, completion: None, resume: None, async_generator: false, strict: true, annexb: Vec::new(), var_level: None, eval_vars: None, prologue: false, simple_params: false });
        for s in &program.body {
            let (f, name, inferred) = match s {
                Statement::FunctionDeclaration(f) => (f, f.id.as_ref().map(|i| i.name.as_str()), ""),
                Statement::ExportDeclaration(e) => match &e.declaration {
                    Declaration::FunctionDeclaration(f) => (f, f.id.as_ref().map(|i| i.name.as_str()), ""),
                    _ => continue,
                },
                Statement::ExportDefaultDeclaration(e) => match &e.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(f) => (f, Some(f.id.as_ref().map_or(DEFAULT_BINDING, |i| i.name.as_str())), "default"),
                    _ => continue,
                },
                _ => continue,
            };
            let Some(name) = name else { continue };
            let n = self.plain_function(f, false, inferred)?;
            self.emit(Insn::CreateClosure(n));
            self.store(name, true);
        }
        for s in &program.body {
            match s {
                Statement::ImportDeclaration(_) | Statement::ExportAllDeclaration(_) | Statement::FunctionDeclaration(_) | Statement::ExportNamedDeclaration(_) | Statement::ExportFromDeclaration(_) => {}
                Statement::ExportDeclaration(e) => match &e.declaration {
                    Declaration::VariableDeclaration(v) => self.declaration(v)?,
                    Declaration::ClassDeclaration(c) => {
                        self.class(c, "")?;
                        if let Some(id) = &c.id {
                            self.store(id.name.as_str(), true);
                        }
                    }
                    Declaration::FunctionDeclaration(_) => {}
                    _ => return Err(self.unsupported(e.span, "declaration")),
                },
                Statement::ExportDefaultDeclaration(e) => match &e.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(_) => {}
                    ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                        self.class(c, "default")?;
                        self.store(c.id.as_ref().map_or(DEFAULT_BINDING, |i| i.name.as_str()), true);
                    }
                    other => {
                        let e = other.as_expression().ok_or_else(|| self.unsupported(e.span, "default export"))?;
                        self.expr_named(e, "default")?;
                        self.store(DEFAULT_BINDING, true);
                    }
                },
                s => self.statement(s)?,
            }
        }
        self.emit(Insn::LdaUndefined);
        self.emit(Insn::Return);
        let f = self.frames.pop().expect("the module's frame");
        self.module[f.index] = f.out;
        Ok(())
    }

    /// The script itself: top-level `var` / function declarations / `let` / `const` are all properties of the global
    /// object.
    /// `local` is the declarations of the eval code's own level (scripts have none); strict eval code puts nothing into
    /// the global.
    #[expect(clippy::too_many_lines, reason = "declaration instantiation follows the spec step by step")]
    fn script(&mut self, program: &'a Program<'a>, local: Option<&Decls<'a>>, strict: bool) -> Result<(), Error> {
        self.module.push(Function::default());
        let eval = local.is_some();
        let out = Function { name: "main".into(), regs: Reg::PARAMS, mode: strict.into(), ..Function::default() };
        self.frames.push(Frame { index: 0, out, top: Reg::PARAMS, envs: 0, controls: Vec::new(), label: None, completion: None, resume: None, async_generator: false, strict, annexb: Vec::new(), var_level: None, eval_vars: None, prologue: false, simple_params: false });
        let completion = self.temp();
        self.frame().completion = Some(completion);
        // Direct eval code uses the call site's `this` (resolved via the outer scope table)
        let direct = !self.levels.is_empty();
        if !direct {
            self.enter(&Decls(vec![Decl { name: THIS, key: Decls::this_key(program.span), kind: DeclKind::This, ready: None }]))?;
        }
        let mut globals = Decls::default();
        if !(eval && strict) {
            globals.vars(&program.body);
            globals.lexical(&program.body);
        }
        // annexB: block-level functions in a script also create a global `var`
        if !strict && !eval {
            let hoisted = Decls::annexb_functions(&program.body, None);
            self.frame().annexb = hoisted.iter().map(|f| f.span.start).collect();
            globals.annexb_globals(&hoisted);
        }
        if let Some(d) = local {
            self.enter(d)?;
        }
        // Sloppy direct eval: a `var` with the same name as a lexical declaration between the call site and the
        // variable environment is a SyntaxError
        if direct && eval && !strict {
            self.eval_conflicts(&globals)?;
        }
        // annexB: block-level functions in eval code also land in the variable environment (not hoisted if they
        // conflict with outer lexical declarations)
        if eval && !strict {
            let mut hoisted = Decls::annexb_functions(&program.body, None);
            hoisted.retain(|f| f.id.as_ref().is_some_and(|id| !self.outer_lexical(id.name.as_str())));
            self.frame().annexb = hoisted.iter().map(|f| f.span.start).collect();
            globals.annexb_globals(&hoisted);
        }
        // Sloppy direct eval: `var` / function declarations land in the calling function (using an existing binding
        // with the same name, otherwise the object holding eval vars)
        if direct && eval && !strict && let Some((vars, statics)) = self.eval_var_level() {
            self.frame().eval_vars = Some(vars);
            for d in &globals.0 {
                match d.kind {
                    DeclKind::Var if !statics.contains(&d.name) => {
                        let obj = self.temp();
                        self.load(vars);
                        self.emit(Insn::Star(obj));
                        let name = self.name(d.name);
                        self.emit(Insn::DeclareEvalVar { obj, name });
                        self.release(obj);
                    }
                    DeclKind::Function(f) => {
                        let n = self.function_declaration(f)?;
                        self.emit(Insn::CreateClosure(n));
                        if statics.contains(&d.name) {
                            self.store(d.name, false);
                        } else {
                            let v = self.temp();
                            self.emit(Insn::Star(v));
                            let obj = self.temp();
                            self.load(vars);
                            self.emit(Insn::Star(obj));
                            self.emit(Insn::Ldar(v));
                            let name = self.name(d.name);
                            self.emit(Insn::DefineNamed { obj, name, kind: PropertyKind::Data, enumerable: true });
                            self.release(obj);
                            self.release(v);
                        }
                    }
                    _ => {}
                }
            }
            globals.0.retain(|d| !matches!(d.kind, DeclKind::Var | DeclKind::Function(_)));
        }
        // First check that all functions and `var`s can be declared; if any cannot, create nothing
        for d in &globals.0 {
            if let DeclKind::Function(_) | DeclKind::Var = d.kind {
                let name = self.name(d.name);
                self.emit(Insn::CheckDeclare { name, function: matches!(d.kind, DeclKind::Function(_)) });
            }
        }
        // Top-level `let` / `const` / `class` of a script are first declared (uninitialized) in the script scope
        for d in globals.0.iter().filter(|_| !eval) {
            if let DeclKind::Let | DeclKind::Const = d.kind {
                self.script_lexicals.insert(d.name);
                let name = self.name(d.name);
                self.emit(Insn::DeclareLexical { name, constant: matches!(d.kind, DeclKind::Const) });
            }
        }
        for d in &globals.0 {
            match d.kind {
                DeclKind::Function(f) => {
                    let n = self.function_declaration(f)?;
                    self.emit(Insn::CreateClosure(n));
                    let name = self.name(d.name);
                    self.emit(Insn::DeclareFunction { name, deletable: eval });
                }
                DeclKind::Var => {
                    let name = self.name(d.name);
                    self.emit(Insn::DeclareVar { name, deletable: eval });
                }
                _ => {}
            }
        }
        // The directive prologue (`'use strict'` etc.) is not in body in oxc, but it consists of expression statements
        // and counts toward the completion value
        for d in &program.directives {
            let k = self.string_const(d.expression.value.as_str(), d.expression.span)?;
            self.emit(Insn::LdaConst(k));
            self.emit(Insn::Star(completion));
        }
        self.statements(&program.body)?;
        self.emit(Insn::Ldar(completion));
        self.emit(Insn::Return);
        self.levels.pop();
        let f = self.frames.pop().expect("the script's frame");
        self.module[0] = f.out;
        Ok(())
    }

    // ---- Statements --------------------------------------------------------------------

    fn statements(&mut self, stmts: &'a [Statement<'a>]) -> Result<(), Error> {
        stmts.iter().try_for_each(|s| self.statement(s))
    }

    fn push_jump(&mut self, label: Option<&'a str>, breakable: bool, continuable: bool) {
        let f = self.frame();
        let envs = f.envs;
        f.controls.push(Control::Jump(Jump { label, breakable, continuable, breaks: Vec::new(), continues: Vec::new(), envs }));
    }

    fn pop_jump(&mut self, brk: Label, cont: Option<Label>) {
        let Some(Control::Jump(j)) = self.frame().controls.pop() else { unreachable!("the innermost control is this jump target") };
        j.breaks.into_iter().for_each(|at| self.patch(at, brk));
        if let Some(c) = cont {
            j.continues.into_iter().for_each(|at| self.patch(at, c));
        }
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

    /// Leaves control stack levels `from` and above: pops the extra environments (`envs` is the landing point's
    /// environment depth), and innermost first removes handlers and closes iterators (async iterators also await the
    /// result of `return`). acc is left as is.
    fn leave_controls(&mut self, from: usize, envs: Option<u16>) {
        if let Some(e) = envs {
            for _ in e..self.frame().envs {
                self.emit(Insn::PopEnv);
            }
        }
        let actions: Vec<Control<'a>> = self.frame().controls[from..]
            .iter()
            .rev()
            .filter_map(|c| match c {
                Control::Handler => Some(Control::Handler),
                Control::Iterator(i) => Some(Control::Iterator(*i)),
                Control::AsyncIterator(i) => Some(Control::AsyncIterator(*i)),
                _ => None,
            })
            .collect();
        for a in actions {
            match a {
                Control::Handler => _ = self.emit(Insn::PopHandler),
                Control::Iterator(iter) => _ = self.emit(Insn::IteratorClose { iter }),
                Control::AsyncIterator(iter) => {
                    let mode = self.frame().resume.expect("for await is inside an async function");
                    let saved = self.temp();
                    self.emit(Insn::Star(saved));
                    self.emit(Insn::AsyncIteratorReturn { iter });
                    self.emit(Insn::Await { mode });
                    // A throw while awaiting `return` propagates outward as usual
                    let v = self.temp();
                    self.emit(Insn::Star(v));
                    let k = self.constant(Constant::Number(f64::from(Resume::Throw)));
                    self.emit(Insn::LdaConst(k));
                    self.emit(Insn::Compare(CompareOp::StrictEq, mode));
                    let skip = self.jump(Insn::JumpIfFalse);
                    self.emit(Insn::Ldar(v));
                    self.emit(Insn::Throw);
                    self.land(skip);
                    self.emit(Insn::Ldar(saved));
                    self.release(saved);
                }
                _ => {}
            }
        }
    }

    /// Jumps into the finally of level `i`, carrying completion type `kind`.
    fn enter_finally(&mut self, i: usize, kind: f64) {
        let Control::Finally(fin) = &self.frame().controls[i] else { unreachable!("a finally") };
        let (envs, kind_reg) = (fin.envs, fin.kind);
        self.leave_controls(i + 1, Some(envs));
        let k = self.constant(Constant::Number(kind));
        self.emit(Insn::LdaConst(k));
        self.emit(Insn::Star(kind_reg));
        let at = self.jump(Insn::Jump);
        let Control::Finally(fin) = &mut self.frame().controls[i] else { unreachable!("a finally") };
        fin.entries.push(at);
    }

    /// `break` / `continue`: pops the extra environments and handlers, closes the iterators passed, then jumps; detours
    /// into any finally in between first.
    fn exit(&mut self, span: Span, label: Option<&'a str>, cont: bool) -> Result<(), Error> {
        match self.exit_plan(Some((label, cont))) {
            Exit::Jump(i) => {
                let Control::Jump(j) = &self.frame().controls[i] else { unreachable!("a jump target") };
                let envs = j.envs;
                self.leave_controls(i + 1, Some(envs));
                let at = self.jump(Insn::Jump);
                let Control::Jump(j) = &mut self.frame().controls[i] else { unreachable!("a jump target") };
                if cont { j.continues.push(at) } else { j.breaks.push(at) }
                Ok(())
            }
            Exit::Finally(i) => {
                let Control::Finally(fin) = &mut self.frame().controls[i] else { unreachable!("a finally") };
                let n = fin.routes.len();
                fin.routes.push(Route { span, label, cont });
                self.enter_finally(i, COMPLETION_ROUTE + f64::from(u32::try_from(n).expect("fewer than 2^32 routes")));
                Ok(())
            }
            Exit::Function => Err(self.unsupported(span, "a jump with no target")),
        }
    }

    /// `return` (value in acc): if a finally is in between, stores the value and detours into it first; closes all
    /// iterators passed.
    pub(crate) fn ret(&mut self) {
        match self.exit_plan(None) {
            Exit::Finally(i) => {
                let Control::Finally(fin) = &self.frame().controls[i] else { unreachable!("a finally") };
                let value = fin.value;
                self.emit(Insn::Star(value));
                self.enter_finally(i, COMPLETION_RETURN);
            }
            _ => {
                self.leave_controls(0, None);
                self.emit(Insn::Return);
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

    /// Dispatches on the completion type: runs `then` if it is `kind`.
    pub(crate) fn on_completion(&mut self, reg: Reg, kind: f64, then: impl FnOnce(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        let k = self.constant(Constant::Number(kind));
        self.emit(Insn::LdaConst(k));
        self.emit(Insn::Compare(CompareOp::StrictEq, reg));
        let skip = self.jump(Insn::JumpIfFalse);
        then(self)?;
        self.land(skip);
        Ok(())
    }

    /// `try` / `catch` / `finally`.
    fn try_statement(&mut self, t: &'a TryStatement<'a>) -> Result<(), Error> {
        let fin = t.finalizer.as_ref().map(|_| {
            let kind = self.temp();
            let value = self.temp();
            let envs = self.frame().envs;
            self.frame().controls.push(Control::Finally(Finally { kind, value, envs, entries: Vec::new(), routes: Vec::new() }));
            let at = self.jump(Insn::PushHandler);
            self.frame().controls.push(Control::Handler);
            (kind, value, at)
        });
        if let Some(c) = &t.handler {
            let at = self.jump(Insn::PushHandler);
            self.frame().controls.push(Control::Handler);
            self.block(&t.block)?;
            self.frame().controls.pop();
            self.emit(Insn::PopHandler);
            let end = self.jump(Insn::Jump);
            self.land(at);
            self.scoped(&Decls::catch_param(c), |s| {
                if let Some(p) = &c.param {
                    let t = s.temp();
                    s.emit(Insn::Star(t));
                    s.bind_pattern(&p.pattern, t, true)?;
                    s.release(t);
                }
                s.block(&c.body)
            })?;
            self.land(end);
        } else {
            self.block(&t.block)?;
        }
        let Some((kind, value, at)) = fin else { return Ok(()) };
        self.frame().controls.pop();
        self.emit(Insn::PopHandler);
        let normal = self.constant(Constant::Number(COMPLETION_NORMAL));
        self.emit(Insn::LdaConst(normal));
        self.emit(Insn::Star(kind));
        let fall = self.jump(Insn::Jump);
        // Entered by a throw: save the exception value
        self.land(at);
        self.emit(Insn::Star(value));
        let throw = self.constant(Constant::Number(COMPLETION_THROW));
        self.emit(Insn::LdaConst(throw));
        self.emit(Insn::Star(kind));
        self.land(fall);
        let Some(Control::Finally(f)) = self.frame().controls.pop() else { unreachable!("the innermost control is this finally") };
        f.entries.into_iter().for_each(|e| self.land(e));
        self.block(t.finalizer.as_ref().expect("checked above"))?;
        self.on_completion(kind, COMPLETION_THROW, |s| {
            s.emit(Insn::Ldar(value));
            s.emit(Insn::Throw);
            Ok(())
        })?;
        self.on_completion(kind, COMPLETION_RETURN, |s| {
            s.emit(Insn::Ldar(value));
            s.ret();
            Ok(())
        })?;
        for (n, r) in (0u32..).zip(f.routes) {
            self.on_completion(kind, COMPLETION_ROUTE + f64::from(n), |s| s.exit(r.span, r.label, r.cont))?;
        }
        self.release(kind);
        Ok(())
    }

    #[expect(clippy::too_many_lines, reason = "one arm per statement kind; splitting it would hurt readability")]
    fn statement(&mut self, s: &'a Statement<'a>) -> Result<(), Error> {
        match s {
            Statement::ExpressionStatement(e) => match self.frame().completion {
                Some(c) => {
                    self.expr(&e.expression)?;
                    self.emit(Insn::Star(c));
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
                    Some(e) => {
                        self.expr(e)?;
                        if self.frame().async_generator {
                            // In an async generator `return v` awaits v first
                            let mode = self.resume_reg(r.span, "return")?;
                            self.emit(Insn::Await { mode });
                            self.resume_dispatch(mode, false)?;
                        }
                    }
                    None => _ = self.emit(Insn::LdaUndefined),
                }
                self.ret();
            }
            Statement::TryStatement(t) => self.try_statement(t)?,
            Statement::ThrowStatement(t) => {
                self.expr(&t.argument)?;
                self.emit(Insn::Throw);
            }
            Statement::IfStatement(i) => {
                self.expr(&i.test)?;
                let skip = self.jump(Insn::JumpIfFalse);
                self.clause(&i.consequent)?;
                if let Some(a) = &i.alternate {
                    let end = self.jump(Insn::Jump);
                    self.land(skip);
                    self.clause(a)?;
                    self.land(end);
                } else {
                    self.land(skip);
                }
            }
            Statement::WhileStatement(w) => {
                let label = self.take_label();
                let top = self.here();
                self.expr(&w.test)?;
                let exit = self.jump(Insn::JumpIfFalse);
                self.push_jump(label, true, true);
                self.statement(&w.body)?;
                self.emit(Insn::Jump(top));
                self.land(exit);
                let end = self.here();
                self.pop_jump(end, Some(top));
            }
            Statement::DoWhileStatement(w) => {
                let label = self.take_label();
                let top = self.here();
                self.push_jump(label, true, true);
                self.statement(&w.body)?;
                let cont = self.here();
                self.expr(&w.test)?;
                self.emit(Insn::JumpIfTrue(top));
                let end = self.here();
                self.pop_jump(end, Some(cont));
            }
            Statement::ForStatement(f) => {
                let label = self.take_label();
                self.scoped(&Decls::for_init(f.init.as_ref()), |c| {
                    match &f.init {
                        Some(ForStatementInit::VariableDeclaration(d)) => c.declaration(d)?,
                        Some(init) => c.expr(init.as_expression().expect("a for-init is a declaration or an expression"))?,
                        None => {}
                    }
                    let top = c.here();
                    let exit = match &f.test {
                        Some(t) => {
                            c.expr(t)?;
                            Some(c.jump(Insn::JumpIfFalse))
                        }
                        None => None,
                    };
                    c.push_jump(label, true, true);
                    c.statement(&f.body)?;
                    let cont = c.here();
                    if c.levels.last().is_some_and(|l| l.env) {
                        // A fresh `let` binding per iteration; closures from the previous iteration keep the old one
                        c.emit(Insn::CloneEnv);
                    }
                    if let Some(u) = &f.update {
                        c.effect(u)?;
                    }
                    c.emit(Insn::Jump(top));
                    if let Some(e) = exit {
                        c.land(e);
                    }
                    let end = c.here();
                    c.pop_jump(end, Some(cont));
                    Ok(())
                })?;
            }
            Statement::WithStatement(w) => {
                self.expr(&w.object)?;
                self.emit(Insn::ToObject);
                let hidden = Hidden::with(self.alloc, w.span);
                self.scoped(&Decls::with_scope(hidden, w.span), |c| {
                    c.store(hidden, true);
                    c.statement(&w.body)
                })?;
            }
            Statement::LabeledStatement(l) => {
                let name = l.label.name.as_str();
                if matches!(l.body, Statement::WhileStatement(_) | Statement::DoWhileStatement(_) | Statement::ForStatement(_) | Statement::ForOfStatement(_) | Statement::ForInStatement(_) | Statement::SwitchStatement(_)) {
                    self.frame().label = Some(name);
                    self.statement(&l.body)?;
                } else {
                    self.push_jump(Some(name), false, false);
                    self.statement(&l.body)?;
                    let end = self.here();
                    self.pop_jump(end, None);
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
                self.emit(Insn::Star(t));
                self.bind_pattern(&v.id, t, true)?;
                self.release(t);
                continue;
            };
            match (&v.init, d.kind) {
                (Some(e), _) => self.expr_named(e, id.name.as_str())?,
                // `let x;` in a loop body must reset to undefined every iteration
                (None, VariableDeclarationKind::Let) => _ = self.emit(Insn::LdaUndefined),
                (None, _) => continue,
            }
            self.store(id.name.as_str(), true);
        }
        Ok(())
    }

    /// `switch`: compares each `case` with strict equality to jump into its body; bodies fall through in order.
    fn switch(&mut self, sw: &'a SwitchStatement<'a>) -> Result<(), Error> {
        let label = self.take_label();
        self.expr(&sw.discriminant)?;
        let d = self.temp();
        self.emit(Insn::Star(d));
        self.scoped(&Decls::block(sw.cases.iter().map(|c| c.consequent.as_slice())), |c| {
            c.levels.last_mut().expect("the switch scope").switch = true;
            let mut entries = Vec::with_capacity(sw.cases.len());
            for case in &sw.cases {
                if let Some(t) = &case.test {
                    c.expr(t)?;
                    c.emit(Insn::Compare(CompareOp::StrictEq, d));
                    entries.push(Some(c.jump(Insn::JumpIfTrue)));
                } else {
                    entries.push(None);
                }
            }
            let fallback = c.jump(Insn::Jump);
            c.push_jump(label, true, false);
            let mut default = None;
            for (case, entry) in sw.cases.iter().zip(entries) {
                match entry {
                    Some(at) => c.land(at),
                    None => default = Some(c.here()),
                }
                c.statements(&case.consequent)?;
            }
            let end = c.here();
            c.patch(fallback, default.unwrap_or(end));
            c.pop_jump(end, None);
            Ok(())
        })?;
        self.release(d);
        Ok(())
    }
}
