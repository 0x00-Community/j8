//! JS ⇒ 字节码：作用域与函数、语句。表达式在 `expr.rs`。
use std::collections::HashSet;

use oxc_allocator::Allocator;
use oxc_ast::ast::{Directive, BindingIdentifier, BindingPattern, BlockStatement, Class, ClassElement, Expression, MethodDefinition, MethodDefinitionKind, PrivateIdentifier, PropertyKey, ForStatementInit, FormalParameters, Function as AstFunction, ForInStatement, ForOfStatement, ForStatementLeft, Program, Statement, SwitchStatement, TryStatement, VariableDeclaration, VariableDeclarationKind};
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, SourceType, Span};
use r8frame::{CodeKind, CompareOp, Error, Resume, Source};

use crate::scope::{Decl, DeclKind, Decls, Hidden, Key, Resolver, THIS};
use crate::{Const, Constant, Function, FunctionKind, Insn, Label, Module, PropertyKind, Reg};

/// 回填前的跳转目标
const UNPATCHED: Label = Label(u32::MAX);

/// finally 的完成类型：正常落下
const COMPLETION_NORMAL: f64 = 0.0;
/// finally 的完成类型：带着异常进来，出去接着抛
const COMPLETION_THROW: f64 = 1.0;
/// finally 的完成类型：带着返回值进来，出去接着返回
const COMPLETION_RETURN: f64 = 2.0;
/// finally 的完成类型：第 n 个绕道的 break / continue 是这个数加 n
const COMPLETION_ROUTE: f64 = 3.0;

/// 一个名字绑在哪儿。
#[derive(Clone, Copy)]
enum Binding {
    Reg(Reg),
    /// 本层环境的槽位
    Env(u16),
}

struct Bound<'a> {
    name: &'a str,
    binding: Binding,
    constant: bool,
    /// 暂时性死区到这个位置结束（`u32::MAX`：总要查）
    ready: Option<u32>,
}

/// 生成代码时的一层作用域。
struct Level<'a> {
    /// 属于第几层函数（`frames` 下标）
    func: usize,
    names: Vec<Bound<'a>>,
    /// 这层有没有堆上的环境
    env: bool,
    /// 进来前的寄存器栈顶，出去时退回
    mark: u16,
    /// `switch` 的那层：分支可能跳过声明，死区检查省不掉
    switch: bool,
}

/// 名字解析的结果：在哪、是不是常量、读写前要不要查死区（给出声明结束的位置）。
#[derive(Clone, Copy)]
pub(crate) struct Resolved<'a> {
    pub place: Place<'a>,
    pub constant: bool,
    /// `Some(ready)`：使用处在 `ready` 之前、或绑定在环境里 / `switch` 里，就要查
    pub tdz: Option<(u32, bool)>,
}

impl Resolved<'_> {
    /// 在 `pos` 处使用要不要查死区。
    pub fn needs_check(&self, pos: u32) -> bool {
        match self.tdz {
            Some((ready, always)) => always || pos < ready,
            None => false,
        }
    }
}

/// 名字的位置。
#[derive(Clone, Copy)]
pub(crate) enum Place<'a> {
    Reg(Reg),
    Env { depth: u16, slot: u16 },
    Global(&'a str),
}

/// `break` / `continue` 的去处。
struct Jump<'a> {
    label: Option<&'a str>,
    /// 不带标签的 `break` 能不能跳出它（循环与 `switch` 能，带标签的块不能）
    breakable: bool,
    continuable: bool,
    breaks: Vec<usize>,
    continues: Vec<usize>,
    /// 进它时本函数有几层环境：跳出去要退掉多出来的
    envs: u16,
}

/// 穿过 finally 的 `break` / `continue`：finally 跑完再接着跳。
#[derive(Clone, Copy)]
struct Route<'a> {
    span: Span,
    label: Option<&'a str>,
    cont: bool,
}

/// 一个 finally：完成类型与值的寄存器、跳进来的地方、绕道的跳转。
struct Finally<'a> {
    kind: Reg,
    value: Reg,
    envs: u16,
    entries: Vec<usize>,
    routes: Vec<Route<'a>>,
}

/// 控制栈的一层：跳转目标、装着的异常处理器、finally。
enum Control<'a> {
    Jump(Jump<'a>),
    Handler,
    Finally(Finally<'a>),
    /// `for-of` 的迭代器（记录在这三个寄存器）：跳出去要先关掉它
    Iterator(Reg),
    /// `for await` 的迭代器（记录在这四个寄存器）：跳出去要先调 `return` 并等它
    AsyncIterator(Reg),
}

/// 函数的种类与风味（生成器、async）。
#[derive(Clone, Copy, Default)]
pub(crate) struct Flavor {
    pub kind: FunctionKind,
    pub generator: bool,
    pub asynchronous: bool,
    /// 自己就是严格的（函数体开头 `'use strict'`、类体里）；外层严格的照样继承
    pub strict: bool,
}

/// 函数节点的风味：生成器 / async 照节点，种类由调用方给。
impl From<(&AstFunction<'_>, FunctionKind)> for Flavor {
    fn from((f, kind): (&AstFunction<'_>, FunctionKind)) -> Self {
        let strict = f.body.as_ref().is_some_and(|b| Self::use_strict(&b.directives));
        Self { kind, generator: f.generator, asynchronous: f.r#async, strict }
    }
}

impl Flavor {
    /// 指令序言里有没有 `'use strict'`（原文正好这样写，不带转义）。
    pub fn use_strict(directives: &[Directive<'_>]) -> bool {
        directives.iter().any(|d| d.directive.as_str() == "use strict")
    }
}

impl From<FunctionKind> for Flavor {
    fn from(kind: FunctionKind) -> Self {
        Self { kind, ..Self::default() }
    }
}

/// 跳出去落到哪：控制栈第几层的跳转目标 / finally，或直接离开函数。
enum Exit {
    Jump(usize),
    Finally(usize),
    Function,
}

/// 正在编的一个函数。
struct Frame<'a> {
    index: usize,
    out: Function,
    /// 下一个空寄存器
    top: u16,
    /// 当前有几层本函数的环境
    envs: u16,
    controls: Vec<Control<'a>>,
    /// 下一个循环 / `switch` 的标签
    label: Option<&'a str>,
    /// 脚本的完成值放这里（只有脚本本身有）
    completion: Option<Reg>,
    /// 生成器 / async 函数恢复时写恢复方式的寄存器
    resume: Option<Reg>,
    /// async 生成器：`yield` / `return` 先等值
    async_generator: bool,
    /// 严格模式
    strict: bool,
    /// annexB：执行到声明处要把值写到外层 `var` 的块级函数（按函数节点的起始位置）
    annexb: Vec<u32>,
    /// 函数层（`var` 所在）的作用域层下标；脚本是全局
    var_level: Option<usize>,
}

/// 函数体：语句、箭头函数的表达式、类的字段初始化、缺省构造器。
#[derive(Clone, Copy)]
pub(crate) enum Body<'a> {
    Statements(&'a [Statement<'a>]),
    Expression(&'a Expression<'a>),
    /// 类的实例（`statik` 为假）或静态字段：逐个在 `this` 上定义
    Fields { class: &'a Class<'a>, statik: bool },
    /// 没写 `constructor` 的类：派生类转手 `super(...arguments)`
    DefaultConstructor { derived: bool },
}

pub(crate) struct Compiler<'a, 's> {
    src: &'s Source<'s>,
    /// AST 的分配器：类作用域的隐藏绑定名（`#x` 等）也分配在这里
    pub(crate) alloc: &'a Allocator,
    /// 被引用到的绑定（`new.target` / `arguments` 用到才准备）
    used: HashSet<Key>,
    /// 标签模板调用点的编号（模板对象按（函数, 编号）缓存）
    pub(crate) sites: u32,
    /// 正在编几层类体（类体里全是严格代码）
    class_depth: u32,
    captured: HashSet<Key>,
    module: Vec<Function>,
    frames: Vec<Frame<'a>>,
    levels: Vec<Level<'a>>,
    /// 脚本顶层的 `let` / `const` / `class`：在脚本作用域里，初始化走 `InitLexical`
    script_lexicals: HashSet<&'a str>,
    /// 正在编的可选链：每条链里 `?.` 遇到 null / undefined 跳到链尾
    pub(crate) chains: Vec<Vec<usize>>,
}

impl TryFrom<&Source<'_>> for Module {
    type Error = Error;

    /// 编一个脚本：最后一条表达式语句的值是返回值。
    fn try_from(src: &Source<'_>) -> Result<Self, Error> {
        Self::compile(src, CodeKind::Script)
    }
}

impl Module {
    /// 按种类编：脚本的顶层词法声明进脚本作用域；eval 代码的只在它自己里面。
    pub fn compile(src: &Source<'_>, kind: CodeKind) -> Result<Self, Error> {
        let allocator = Allocator::default();
        // 脚本（非模块）；正则字面量解析时就校验
        let options = ParseOptions { parse_regular_expression: true, ..ParseOptions::default() };
        let ret = Parser::new(&allocator, src.text, SourceType::script()).with_options(options).parse();
        if let Some(d) = ret.diagnostics.first() {
            return Err(Error::Syntax(format!("{}: {d}", src.file)));
        }
        let program = &ret.program;
        // 早期错误（重复声明、非法的 break / super / new.target……）：oxc 的语法分析不管，要 semantic 检查
        let checked = SemanticBuilder::new().with_check_syntax_error(true).build(program);
        if let Some(d) = checked.diagnostics.errors().next() {
            return Err(Error::Syntax(format!("{}: {d}", src.file)));
        }
        let strict = kind == CodeKind::StrictEval || Flavor::use_strict(&program.directives);
        let local = (kind != CodeKind::Script).then(|| Decls::eval_local(&program.body, strict));
        let (captured, used) = Resolver::script(&program.body, Decls::this_key(program.span), &allocator, local.as_ref(), strict);
        let mut c = Compiler { src, alloc: &allocator, captured, used, sites: 0, class_depth: 0, module: Vec::new(), frames: Vec::new(), levels: Vec::new(), script_lexicals: HashSet::new(), chains: Vec::new() };
        c.script(program, local.as_ref(), strict)?;
        Ok(Module { functions: c.module })
    }
}

impl<'a> Compiler<'a, '_> {
    // ---- 基础 --------------------------------------------------------------------------

    pub(crate) fn unsupported(&self, span: Span, what: &str) -> Error {
        self.src.unsupported(span.start, what)
    }

    /// annexB：块级函数声明执行到这里，把块里的函数值写到外层的 `var`（函数层绑定或全局）。
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
                        };
                        self.emit(insn);
                        return;
                    }
                    if l.env {
                        depth += 1;
                    }
                }
            }
            None => {
                let k = self.name(name);
                self.emit(Insn::StaGlobal { name: k, strict: false });
            }
        }
    }

    /// 这里的代码是不是严格的：函数本身严格，或者在类体里（`extends` 与计算键在外层函数里求值，也是严格代码）。
    pub(crate) fn strict_here(&self) -> bool {
        self.strict() || self.class_depth > 0
    }

    /// 当前函数是不是严格模式。
    pub(crate) fn strict(&self) -> bool {
        self.frames.last().is_some_and(|f| f.strict)
    }

    fn frame(&mut self) -> &mut Frame<'a> {
        self.frames.last_mut().expect("inside a function")
    }

    pub(crate) fn emit(&mut self, i: Insn) -> usize {
        let out = &mut self.frame().out;
        out.insns.push(i);
        out.insns.len() - 1
    }

    pub(crate) fn here(&mut self) -> Label {
        Label(u32::try_from(self.frame().out.insns.len()).expect("fewer than 2^32 instructions"))
    }

    /// 发一条目标待回填的跳转。
    pub(crate) fn jump(&mut self, mk: fn(Label) -> Insn) -> usize {
        self.emit(mk(UNPATCHED))
    }

    pub(crate) fn patch(&mut self, at: usize, to: Label) {
        *self.frame().out.insns[at].target_mut().expect("a jump") = to;
    }

    /// 把 `at` 处的跳转指到这里。
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

    /// 连续 `n` 个临时寄存器，按栈的次序 [`release`](Self::release)。
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

    // ---- 作用域 ------------------------------------------------------------------------

    /// 进一层作用域：分配绑定（被捕获的进环境），拷进被捕获的 `this` / 形参，建函数声明的闭包。
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
            names.push(Bound { name: d.name, binding, constant: matches!(d.kind, DeclKind::Const), ready: d.ready });
        }
        let env = slots > 0;
        if env {
            self.emit(Insn::PushEnv(slots));
            self.frame().envs += 1;
        }
        let bindings: Vec<Binding> = names.iter().map(|n| n.binding).collect();
        self.levels.push(Level { func, names, env, mark, switch: false });
        // 暂时性死区：槽位先填未初始化标记
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
                    // 建 arguments 要全部实参
                    self.frame().out.keeps_args = true;
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

    /// 把 acc 存进本层刚分配的绑定。
    fn bind(&mut self, b: Binding) {
        match b {
            Binding::Reg(r) => self.emit(Insn::Star(r)),
            Binding::Env(slot) => self.emit(Insn::StaEnv { depth: 0, slot }),
        };
    }

    /// 出一层块作用域。
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

    /// 名字解析：由内向外，数经过几层环境；找不到就是全局。
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
                };
                return Resolved { place, constant: n.constant, tdz: n.ready.map(|r| (r, in_env || l.switch)) };
            }
            if l.env {
                depth += 1;
            }
        }
        Resolved { place: Place::Global(name), constant: false, tdz: None }
    }

    pub(crate) fn lookup(&self, name: &'a str) -> (Place<'a>, bool) {
        let r = self.resolve(name);
        (r.place, r.constant)
    }

    /// 读一个名字进 acc（不知道使用位置，死区按要查算）。
    pub(crate) fn load(&mut self, name: &'a str) {
        self.load_at(name, 0);
    }

    /// 在源码位置 `pos` 读一个名字进 acc。`undefined` / `NaN` / `Infinity` 没被遮住就是常量。
    pub(crate) fn load_at(&mut self, name: &'a str, pos: u32) {
        let r = self.resolve(name);
        let insn = match r.place {
            Place::Reg(reg) => Insn::Ldar(reg),
            Place::Env { depth, slot } => Insn::LdaEnv { depth, slot },
            Place::Global("undefined") => Insn::LdaUndefined,
            Place::Global("NaN") => Insn::LdaConst(self.constant(Constant::Number(f64::NAN))),
            Place::Global("Infinity") => Insn::LdaConst(self.constant(Constant::Number(f64::INFINITY))),
            Place::Global(g) => Insn::LdaGlobal(self.name(g)),
        };
        self.emit(insn);
        if r.needs_check(pos) {
            let check = if name == THIS { Insn::CheckThis } else { Insn::CheckInit { name: self.name(name) } };
            self.emit(check);
        }
    }

    /// 把 acc 存进一个名字；`init` 是声明里的初始化（`const` 只许这一次，也不查死区）。
    pub(crate) fn store(&mut self, name: &'a str, init: bool) {
        let r = self.resolve(name);
        if r.constant && !init {
            self.emit(Insn::ThrowConstAssign);
            return;
        }
        if !init && r.needs_check(0) {
            let k = self.name(name);
            match r.place {
                Place::Reg(reg) => _ = self.emit(Insn::CheckReg { reg, name: k }),
                Place::Env { depth, slot } => _ = self.emit(Insn::CheckEnv { depth, slot, name: k }),
                Place::Global(_) => {}
            }
        }
        let insn = match r.place {
            Place::Reg(reg) => Insn::Star(reg),
            Place::Env { depth, slot } => Insn::StaEnv { depth, slot },
            Place::Global(g) if init && self.script_lexicals.contains(g) => Insn::InitLexical { name: self.name(g) },
            Place::Global(g) => Insn::StaGlobal { name: self.name(g), strict: self.strict_here() },
        };
        self.emit(insn);
    }

    // ---- 函数 --------------------------------------------------------------------------

    /// 编一个函数，返回它在模块里的下标。
    pub(crate) fn function(&mut self, name: &str, flavor: Flavor, this: Option<Key>, self_name: Option<&'a BindingIdentifier<'a>>, params: Option<&'a FormalParameters<'a>>, body: Body<'a>) -> Result<u32, Error> {
        let count = params.map_or(0, |p| p.items.len());
        let n = u16::try_from(count).map_err(|_| self.unsupported(params.map_or(Span::default(), |p| p.span), "more than 65535 parameters"))?;
        let index = self.module.len();
        self.module.push(Function::default());
        let top = Reg::PARAMS + n;
        let keeps_args = matches!(body, Body::DefaultConstructor { derived: true }) || params.is_some_and(|p| p.rest.is_some());
        // length：第一个带默认值的形参之前的个数（剩余参数不算）
        let length = u16::try_from(params.map_or(0, |p| p.items.iter().take_while(|i| i.initializer.is_none()).count())).unwrap_or(u16::MAX);
        // 类里的函数（方法、构造器、字段初始化）都是严格的
        let strict = flavor.strict || self.class_depth > 0 || self.frames.last().is_some_and(|f| f.strict);
        let out = Function { name: name.into(), kind: flavor.kind, keeps_args, generator: flavor.generator, asynchronous: flavor.asynchronous, mode: strict.into(), params: n, length, regs: top, ..Function::default() };
        self.frames.push(Frame { index, out, top, envs: 0, controls: Vec::new(), label: None, completion: None, resume: None, async_generator: false, strict, annexb: Vec::new(), var_level: None });
        if flavor.generator || flavor.asynchronous {
            let mode = self.temp();
            self.frame().resume = Some(mode);
            self.frame().async_generator = flavor.generator && flavor.asynchronous;
        }
        let stmts = match body {
            Body::Statements(s) => s,
            _ => &[],
        };
        self.enter(&Decls::function(this, self_name, params, stmts, !strict))?;
        if !strict {
            self.frame().var_level = Some(self.levels.len() - 1);
            self.frame().annexb = Decls::annexb_functions(stmts, params).iter().map(|f| f.span.start).collect();
        }
        if flavor.kind == (FunctionKind::Constructor { derived: true })
            && let Some(t) = self.levels.last_mut().and_then(|l| l.names.iter_mut().find(|n| n.name == THIS))
        {
            // 派生类构造器的 `this` 在 `super()` 之前没初始化
            t.ready = Some(u32::MAX);
        }
        if let Some(p) = params {
            self.prologue(p)?;
        }
        // 生成器：形参与声明都准备好了才挂起（调用时就跑到这里，形参出错同步抛），第一次 next 从这里接着跑
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

    /// 形参序言：默认值、解构形参、剩余参数。
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
                    // 被捕获的形参进帧时已拷进环境，默认值算完要再拷一次
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

    /// 把寄存器 `value` 里的值按模式绑到名字上（`init`：声明里的初始化）。
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
                // 默认值等抛出时要先关迭代器
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

    /// 跑 `f`；期间抛出就先关掉迭代器 `iter`（没走完才调 `return`）再接着抛。
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

    /// 取寄存器 `value` 的迭代器：迭代器记录占三个连续寄存器（调用方释放）。
    pub(crate) fn iterate(&mut self, value: Reg) -> Reg {
        self.emit(Insn::Ldar(value));
        let iter = self.temps(3);
        self.emit(Insn::GetIterator { dst: iter });
        iter
    }

    /// `for-of` / `for-in` 的左边：声明（含解构）或赋值目标，值在 `v`。
    fn for_left(&mut self, left: &'a ForStatementLeft<'a>, v: Reg) -> Result<(), Error> {
        match left {
            ForStatementLeft::VariableDeclaration(d) => {
                let decl = d.declarations.first().ok_or_else(|| self.unsupported(d.span, "an empty declaration"))?;
                self.bind_pattern(&decl.id, v, true)
            }
            t => self.assign_target(t.as_assignment_target().ok_or_else(|| self.unsupported(t.span(), "for-in / for-of target"))?, v),
        }
    }

    /// `for (left of right) body`：迭代器协议；`break` / `return` / 抛出都关掉迭代器。
    /// `for await (left of right) body`：async 迭代器（没有就退回同步迭代器并等每个值），每步等 `next()` 的结果；
    /// `break` / `return` 穿出去时调 `return` 并等它，循环体抛出时关掉迭代器。
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
        // 退回同步迭代器时值本身也要等
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
        // break 落这里：调 return 并等它
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
        // break 落这里：关迭代器
        let broke = self.here();
        self.emit(Insn::IteratorClose { iter });
        let end = self.jump(Insn::Jump);
        // 循环体抛出：关迭代器再接着抛
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

    /// `for (left in right) body`：先取键表快照，按下标走。
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

    /// `yield` / `await` 之后：按恢复方式分派（`return` 经过 finally、`throw` 在这里抛），否则 acc 是送进来的值。
    pub(crate) fn resume_dispatch(&mut self, mode: Reg, returnable: bool) -> Result<(), Error> {
        let v = self.temp();
        self.emit(Insn::Star(v));
        if returnable {
            self.on_completion(mode, f64::from(Resume::Return), |s| {
                s.emit(Insn::Ldar(v));
                if s.frame().async_generator {
                    // async 生成器的 return(v) 先等 v
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

    /// 生成器 / async 函数恢复方式的寄存器；不在里面就报错。
    pub(crate) fn resume_reg(&mut self, span: Span, what: &str) -> Result<Reg, Error> {
        self.frame().resume.ok_or_else(|| self.unsupported(span, what))
    }

    /// `value` 是 `undefined` 就换成默认值：结果放一个新临时寄存器（调用方释放）。
    /// `name` 是默认值是匿名函数时推断给它的名字。
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

    /// 解构 `null` / `undefined` 抛 TypeError（`{} = v` 没有属性可读时补这一下）。
    pub(crate) fn require_object(&mut self, value: Reg) {
        let k = self.name("constructor");
        self.emit(Insn::GetNamed { obj: value, name: k });
    }

    /// 属性键求值放进寄存器 `k`（静态键是字符串常量）。
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

    /// 普通函数（声明或表达式）：查掉不支持的种类。
    /// 普通函数；匿名的用推断出的名字 `inferred`。
    pub(crate) fn plain_function(&mut self, f: &'a AstFunction<'a>, self_name: bool, inferred: &str) -> Result<u32, Error> {
        let flavor = Flavor::from((f, FunctionKind::Normal));
        let body = f.body.as_ref().ok_or_else(|| self.unsupported(f.span, "a function without a body"))?;
        let name = f.id.as_ref().map_or(inferred, |id| id.name.as_str());
        self.function(name, flavor, Some(Decls::this_key(f.span)), if self_name { f.id.as_ref() } else { None }, Some(&f.params), Body::Statements(&body.statements))
    }

    /// 方法（类方法、对象字面量方法、访问器）：不可 `new`。
    pub(crate) fn method(&mut self, name: &str, f: &'a AstFunction<'a>, kind: FunctionKind) -> Result<u32, Error> {
        let flavor = Flavor::from((f, kind));
        let body = f.body.as_ref().ok_or_else(|| self.unsupported(f.span, "a method without a body"))?;
        self.function(name, flavor, Some(Decls::this_key(f.span)), None, Some(&f.params), Body::Statements(&body.statements))
    }

    /// 字段初始化函数的函数体：实例的先装私有方法 / 访问器，再按顺序定义字段（公开的可枚举、私有的进私有表）；
    /// 静态的还按顺序跑静态块。
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

    /// 方法定义的种类 ⇒ 属性的种类。
    fn method_kind(kind: MethodDefinitionKind) -> PropertyKind {
        match kind {
            MethodDefinitionKind::Get => PropertyKind::Getter,
            MethodDefinitionKind::Set => PropertyKind::Setter,
            MethodDefinitionKind::Method | MethodDefinitionKind::Constructor => PropertyKind::Data,
        }
    }

    /// 非计算的属性键的文本（函数名推断用；私有名带 `#`）；计算键给 `None`。
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

    /// 方法 / 访问器的函数名：键名，访问器前面加 `get ` / `set `；计算键给空（运行时才知道）。
    pub(crate) fn method_name(&self, key: &'a PropertyKey<'a>, computed: bool, kind: MethodDefinitionKind) -> &'a str {
        let Some(n) = self.static_name(key, computed) else { return "" };
        match kind {
            MethodDefinitionKind::Get => self.alloc.alloc_str(&format!("get {n}")),
            MethodDefinitionKind::Set => self.alloc.alloc_str(&format!("set {n}")),
            MethodDefinitionKind::Method | MethodDefinitionKind::Constructor => n,
        }
    }

    /// 非计算的属性键（标识符 / 字符串 / 数）的常量；计算键给 `None`。
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

    /// 类：构造器 + 原型方法 + 静态方法 + 字段初始化函数。结果（构造器）在 acc。
    /// 类里每个私有名一个新的私有键（getter / setter 共用一个），存进类作用域的绑定。
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

    /// 私有方法 / 访问器：函数只建一次存进隐藏绑定；静态的现在就装到构造器（`target`）上，实例的构造时装。
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

    /// 类；匿名的构造器用推断出的名字 `inferred`。
    pub(crate) fn class(&mut self, c: &'a Class<'a>, inferred: &'a str) -> Result<(), Error> {
        // 类体（含 extends）全是严格代码
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
        // 类体里的类名：构造器一建好就初始化（静态块、静态字段里就能用）
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
                        // 计算字段名在类定义时求一次
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

    /// 脚本本身：顶层 `var` / 函数声明 / `let` / `const` 都是全局对象的属性。
    /// `local` 是 eval 代码自己那一层的声明（脚本没有）；严格的 eval 代码什么都不落到全局。
    fn script(&mut self, program: &'a Program<'a>, local: Option<&Decls<'a>>, strict: bool) -> Result<(), Error> {
        self.module.push(Function::default());
        let eval = local.is_some();
        let out = Function { name: "main".into(), regs: Reg::PARAMS, mode: strict.into(), ..Function::default() };
        self.frames.push(Frame { index: 0, out, top: Reg::PARAMS, envs: 0, controls: Vec::new(), label: None, completion: None, resume: None, async_generator: false, strict, annexb: Vec::new(), var_level: None });
        let completion = self.temp();
        self.frame().completion = Some(completion);
        self.enter(&Decls(vec![Decl { name: THIS, key: Decls::this_key(program.span), kind: DeclKind::This, ready: None }]))?;
        let mut globals = Decls::default();
        if !(eval && strict) {
            globals.vars(&program.body);
            globals.lexical(&program.body);
        }
        // annexB：脚本里块级函数也在全局建 `var`
        if !strict && !eval {
            let hoisted = Decls::annexb_functions(&program.body, None);
            self.frame().annexb = hoisted.iter().map(|f| f.span.start).collect();
            globals.annexb_globals(&hoisted);
        }
        if let Some(d) = local {
            self.enter(d)?;
        }
        // 先查全部函数、`var` 能不能声明，一个不行就什么都不建
        for d in &globals.0 {
            if let DeclKind::Function(_) | DeclKind::Var = d.kind {
                let name = self.name(d.name);
                self.emit(Insn::CheckDeclare { name, function: matches!(d.kind, DeclKind::Function(_)) });
            }
        }
        // 脚本顶层的 `let` / `const` / `class` 先在脚本作用域里声明（未初始化）
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
        // 指令序言（`'use strict'` 等）在 oxc 里不在 body 里，但也是表达式语句，算完成值
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

    // ---- 语句 --------------------------------------------------------------------------

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

    /// 循环 / `switch` 用掉挂着的标签。
    fn take_label(&mut self) -> Option<&'a str> {
        self.frame().label.take()
    }

    /// 由内向外找跳出去的落点：先碰到 finally 就绕进它；`target` 为 `None` 是 `return`。
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

    /// 离开控制栈第 `from` 层及以上：退掉多进的环境（`envs` 是落点的环境层数），由内向外卸处理器、关迭代器
    /// （async 迭代器还要等 `return` 的结果）。acc 原样留着。
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
                    // 等 `return` 时抛出照样往外抛
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

    /// 跳进第 `i` 层的 finally，带上完成类型 `kind`。
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

    /// `break` / `continue`：退掉多进的环境与处理器、关掉经过的迭代器再跳；中间隔着 finally 就先绕进它。
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

    /// `return`（值在 acc）：中间隔着 finally 就先把值存好绕进它；经过的迭代器都关掉。
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

    /// `if` 的分支：直接是函数声明的当成只有它的块（annexB B.3.3）。
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

    /// 按完成类型分派：是 `kind` 就做 `then`。
    pub(crate) fn on_completion(&mut self, reg: Reg, kind: f64, then: impl FnOnce(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        let k = self.constant(Constant::Number(kind));
        self.emit(Insn::LdaConst(k));
        self.emit(Insn::Compare(CompareOp::StrictEq, reg));
        let skip = self.jump(Insn::JumpIfFalse);
        then(self)?;
        self.land(skip);
        Ok(())
    }

    /// `try` / `catch` / `finally`。
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
        // 抛出进来：存好异常值
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

    #[expect(clippy::too_many_lines, reason = "一种语句一个分支，拆开反而难读")]
    fn statement(&mut self, s: &'a Statement<'a>) -> Result<(), Error> {
        match s {
            Statement::ExpressionStatement(e) => {
                self.expr(&e.expression)?;
                if let Some(c) = self.frame().completion {
                    self.emit(Insn::Star(c));
                }
            }
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
                            // async 生成器里 `return v` 先等 v
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
                        // 每轮一份新的 `let` 绑定，上一轮的闭包留着旧的
                        c.emit(Insn::CloneEnv);
                    }
                    if let Some(u) = &f.update {
                        c.expr(u)?;
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
                // 循环体里的 `let x;` 每轮都要回到 undefined
                (None, VariableDeclarationKind::Let) => _ = self.emit(Insn::LdaUndefined),
                (None, _) => continue,
            }
            self.store(id.name.as_str(), true);
        }
        Ok(())
    }

    /// `switch`：逐个 `case` 严格相等比较跳进分支体，分支体按顺序落下去。
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
