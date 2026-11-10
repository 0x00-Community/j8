//! JS ⇒ 字节码：作用域与函数、语句。表达式在 `expr.rs`。
use std::collections::HashSet;

use oxc_allocator::Allocator;
use oxc_ast::ast::{BindingIdentifier, BindingPattern, BlockStatement, Class, ClassElement, Expression, MethodDefinitionKind, PropertyKey, ForStatementInit, FormalParameters, Function as AstFunction, Program, Statement, SwitchStatement, TryStatement, VariableDeclaration, VariableDeclarationKind};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};
use r8frame::{CompareOp, Error, Source};

use crate::scope::{Decl, DeclKind, Decls, Key, Resolver, THIS};
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
}

/// 名字解析的结果。
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
}

/// 跳出去要做的事：退几层环境、卸几个处理器、落到哪。
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
    captured: HashSet<Key>,
    module: Vec<Function>,
    frames: Vec<Frame<'a>>,
    levels: Vec<Level<'a>>,
    /// 脚本顶层的 `const`（是全局对象的属性，但不许再赋值）
    global_consts: HashSet<&'a str>,
    /// 正在编的可选链：每条链里 `?.` 遇到 null / undefined 跳到链尾
    pub(crate) chains: Vec<Vec<usize>>,
}

impl TryFrom<&Source<'_>> for Module {
    type Error = Error;

    /// 编一个脚本：最后一条表达式语句的值是返回值。
    fn try_from(src: &Source<'_>) -> Result<Self, Error> {
        let allocator = Allocator::default();
        let ret = Parser::new(&allocator, src.text, SourceType::cjs()).parse();
        if let Some(d) = ret.diagnostics.first() {
            return Err(Error::Syntax(format!("{}: {d}", src.file)));
        }
        let program = &ret.program;
        let captured = Resolver::script(&program.body, Decls::this_key(program.span));
        let mut c = Compiler { src, captured, module: Vec::new(), frames: Vec::new(), levels: Vec::new(), global_consts: HashSet::new(), chains: Vec::new() };
        c.script(program)?;
        Ok(Module { functions: c.module })
    }
}

impl<'a> Compiler<'a, '_> {
    // ---- 基础 --------------------------------------------------------------------------

    pub(crate) fn unsupported(&self, span: Span, what: &str) -> Error {
        self.src.unsupported(span.start, what)
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
            names.push(Bound { name: d.name, binding, constant: matches!(d.kind, DeclKind::Const) });
        }
        let env = slots > 0;
        if env {
            self.emit(Insn::PushEnv(slots));
            self.frame().envs += 1;
        }
        let bindings: Vec<Binding> = names.iter().map(|n| n.binding).collect();
        self.levels.push(Level { func, names, env, mark });
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
                DeclKind::Var | DeclKind::Let | DeclKind::Const => None,
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
    pub(crate) fn lookup(&self, name: &'a str) -> (Place<'a>, bool) {
        let cur = self.frames.len() - 1;
        let mut depth = 0;
        for l in self.levels.iter().rev() {
            if let Some(n) = l.names.iter().find(|n| n.name == name) {
                let place = match n.binding {
                    Binding::Reg(r) => {
                        assert_eq!(l.func, cur, "an outer function's binding `{name}` used from inside must be captured");
                        Place::Reg(r)
                    }
                    Binding::Env(slot) => Place::Env { depth, slot },
                };
                return (place, n.constant);
            }
            if l.env {
                depth += 1;
            }
        }
        (Place::Global(name), false)
    }

    /// 读一个名字进 acc。`undefined` / `NaN` / `Infinity` 没被遮住就是常量。
    pub(crate) fn load(&mut self, name: &'a str) {
        let insn = match self.lookup(name).0 {
            Place::Reg(r) => Insn::Ldar(r),
            Place::Env { depth, slot } => Insn::LdaEnv { depth, slot },
            Place::Global("undefined") => Insn::LdaUndefined,
            Place::Global("NaN") => Insn::LdaConst(self.constant(Constant::Number(f64::NAN))),
            Place::Global("Infinity") => Insn::LdaConst(self.constant(Constant::Number(f64::INFINITY))),
            Place::Global(g) => Insn::LdaGlobal(self.name(g)),
        };
        self.emit(insn);
    }

    /// 把 acc 存进一个名字；`init` 是声明里的初始化（`const` 只许这一次）。
    pub(crate) fn store(&mut self, name: &'a str, init: bool) {
        let (place, constant) = self.lookup(name);
        let constant = constant || matches!(place, Place::Global(g) if self.global_consts.contains(g));
        if constant && !init {
            let k = self.constant(Constant::String("TypeError: Assignment to constant variable.".into()));
            self.emit(Insn::LdaConst(k));
            self.emit(Insn::Throw);
            return;
        }
        let insn = match place {
            Place::Reg(r) => Insn::Star(r),
            Place::Env { depth, slot } => Insn::StaEnv { depth, slot },
            Place::Global(g) => Insn::StaGlobal(self.name(g)),
        };
        self.emit(insn);
    }

    // ---- 函数 --------------------------------------------------------------------------

    /// 编一个函数，返回它在模块里的下标。
    pub(crate) fn function(&mut self, name: &str, kind: FunctionKind, this: Option<Key>, self_name: Option<&'a BindingIdentifier<'a>>, params: Option<&'a FormalParameters<'a>>, body: Body<'a>) -> Result<u32, Error> {
        let count = params.map_or(0, |p| p.items.len());
        let n = u16::try_from(count).map_err(|_| self.unsupported(params.map_or(Span::default(), |p| p.span), "more than 65535 parameters"))?;
        let index = self.module.len();
        self.module.push(Function::default());
        let top = Reg::PARAMS + n;
        let keeps_args = matches!(body, Body::DefaultConstructor { derived: true }) || params.is_some_and(|p| p.rest.is_some());
        let out = Function { name: name.into(), kind, keeps_args, params: n, regs: top, ..Function::default() };
        self.frames.push(Frame { index, out, top, envs: 0, controls: Vec::new(), label: None, completion: None });
        let stmts = match body {
            Body::Statements(s) => s,
            _ => &[],
        };
        self.enter(&Decls::function(this, self_name, params, stmts))?;
        if let Some(p) = params {
            self.prologue(p)?;
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
                self.expr(init)?;
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
                let t = self.with_default(value, &a.right)?;
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
                self.emit(Insn::Ldar(value));
                self.emit(Insn::ToArray);
                let arr = self.temp();
                self.emit(Insn::Star(arr));
                for (i, e) in (0u32..).zip(&a.elements) {
                    let Some(e) = e else { continue };
                    let k = self.constant(Constant::Number(f64::from(i)));
                    self.emit(Insn::LdaConst(k));
                    self.emit(Insn::GetKeyed { obj: arr });
                    let t = self.temp();
                    self.emit(Insn::Star(t));
                    self.bind_pattern(e, t, init)?;
                    self.release(t);
                }
                if let Some(r) = &a.rest {
                    let from = u16::try_from(a.elements.len()).map_err(|_| self.unsupported(a.span, "more than 65535 elements"))?;
                    self.emit(Insn::SliceFrom { arr, from });
                    let t = self.temp();
                    self.emit(Insn::Star(t));
                    self.bind_pattern(&r.argument, t, init)?;
                    self.release(t);
                }
                self.release(arr);
            }
        }
        Ok(())
    }

    /// `value` 是 `undefined` 就换成默认值：结果放一个新临时寄存器（调用方释放）。
    pub(crate) fn with_default(&mut self, value: Reg, default: &'a Expression<'a>) -> Result<Reg, Error> {
        let t = self.temp();
        self.emit(Insn::Ldar(value));
        self.emit(Insn::Star(t));
        let skip = self.jump(Insn::JumpIfNotUndefined);
        self.expr(default)?;
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
    pub(crate) fn plain_function(&mut self, f: &'a AstFunction<'a>, self_name: bool) -> Result<u32, Error> {
        if f.generator || f.r#async {
            return Err(self.unsupported(f.span, "generators / async functions"));
        }
        let body = f.body.as_ref().ok_or_else(|| self.unsupported(f.span, "a function without a body"))?;
        let name = f.id.as_ref().map_or("", |id| id.name.as_str());
        self.function(name, FunctionKind::Normal, Some(Decls::this_key(f.span)), if self_name { f.id.as_ref() } else { None }, Some(&f.params), Body::Statements(&body.statements))
    }

    /// 方法（类方法、对象字面量方法、访问器）：不可 `new`。
    pub(crate) fn method(&mut self, name: &str, f: &'a AstFunction<'a>, kind: FunctionKind) -> Result<u32, Error> {
        if f.generator || f.r#async {
            return Err(self.unsupported(f.span, "generator / async methods"));
        }
        let body = f.body.as_ref().ok_or_else(|| self.unsupported(f.span, "a method without a body"))?;
        self.function(name, kind, Some(Decls::this_key(f.span)), None, Some(&f.params), Body::Statements(&body.statements))
    }

    /// 字段初始化函数的函数体：`this.k = v` 逐个定义（可枚举）。
    fn fields(&mut self, class: &'a Class<'a>, statik: bool) -> Result<(), Error> {
        for e in &class.body.body {
            let ClassElement::PropertyDefinition(p) = e else { continue };
            if p.r#static != statik {
                continue;
            }
            let name = self.static_key(&p.key, p.computed).ok_or_else(|| self.unsupported(p.span, "computed field names"))??;
            self.load(THIS);
            let this = self.temp();
            self.emit(Insn::Star(this));
            match &p.value {
                Some(v) => self.expr(v)?,
                None => _ = self.emit(Insn::LdaUndefined),
            }
            self.emit(Insn::DefineNamed { obj: this, name, kind: PropertyKind::Data, enumerable: true });
            self.release(this);
        }
        Ok(())
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
    pub(crate) fn class(&mut self, c: &'a Class<'a>, expression: bool) -> Result<(), Error> {
        let name = c.id.as_ref().map_or("", |id| id.name.as_str());
        let parent = match &c.heritage {
            Some(h) => {
                self.expr(&h.expression)?;
                let p = self.temp();
                self.emit(Insn::Star(p));
                Some(p)
            }
            None => None,
        };
        let named = expression && c.id.is_some();
        if named {
            self.enter(&Decls::class_name(c))?;
        }
        let derived = parent.is_some();
        let ctor_def = c.body.body.iter().find_map(|e| match e {
            ClassElement::MethodDefinition(m) if m.kind == MethodDefinitionKind::Constructor => Some(m),
            _ => None,
        });
        let kind = FunctionKind::Constructor { derived };
        let func = match ctor_def {
            Some(m) => self.method(name, &m.value, kind)?,
            None => self.function(name, kind, None, None, None, Body::DefaultConstructor { derived })?,
        };
        let proto = self.temp();
        self.emit(Insn::CreateClass { func, parent, proto });
        let ctor = self.temp();
        self.emit(Insn::Star(ctor));
        let (mut instance_fields, mut static_fields) = (false, false);
        for e in &c.body.body {
            match e {
                ClassElement::MethodDefinition(m) if m.kind != MethodDefinitionKind::Constructor => {
                    let target = if m.r#static { ctor } else { proto };
                    let kind = match m.kind {
                        MethodDefinitionKind::Get => PropertyKind::Getter,
                        MethodDefinitionKind::Set => PropertyKind::Setter,
                        _ => PropertyKind::Data,
                    };
                    match self.static_key(&m.key, m.computed) {
                        Some(name) => {
                            let name = name?;
                            let f = self.method("", &m.value, FunctionKind::Method)?;
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
                    if p.r#static { static_fields = true } else { instance_fields = true }
                }
                ClassElement::StaticBlock(b) => return Err(self.unsupported(b.span, "static blocks")),
                e => return Err(self.unsupported(e.span(), "class element")),
            }
        }
        if instance_fields {
            let f = self.function("", FunctionKind::Method, Some(Decls::this_key(c.body.span)), None, None, Body::Fields { class: c, statik: false })?;
            self.emit(Insn::CreateMethod { func: f, home: proto });
            self.emit(Insn::SetClassFields { ctor });
        }
        if static_fields {
            let f = self.function("", FunctionKind::Method, Some(Decls::this_key(c.span)), None, None, Body::Fields { class: c, statik: true })?;
            self.emit(Insn::CreateMethod { func: f, home: ctor });
            let callee = self.temps(2);
            self.emit(Insn::Star(callee));
            self.emit(Insn::Ldar(ctor));
            self.emit(Insn::Star(Reg(callee.0 + 1)));
            self.emit(Insn::Call { callee, this: Reg(callee.0 + 1), args: Reg(callee.0 + 2), argc: 0 });
            self.release(callee);
        }
        self.emit(Insn::Ldar(ctor));
        if named {
            self.store(name, true);
            self.leave();
        } else {
            self.release(proto);
        }
        if let Some(p) = parent {
            self.release(p);
        }
        Ok(())
    }

    fn function_declaration(&mut self, f: &'a AstFunction<'a>) -> Result<u32, Error> {
        self.plain_function(f, false)
    }

    /// 脚本本身：顶层 `var` / 函数声明 / `let` / `const` 都是全局对象的属性。
    fn script(&mut self, program: &'a Program<'a>) -> Result<(), Error> {
        self.module.push(Function::default());
        let out = Function { name: "main".into(), regs: Reg::PARAMS, ..Function::default() };
        self.frames.push(Frame { index: 0, out, top: Reg::PARAMS, envs: 0, controls: Vec::new(), label: None, completion: None });
        let completion = self.temp();
        self.frame().completion = Some(completion);
        self.enter(&Decls(vec![Decl { name: THIS, key: Decls::this_key(program.span), kind: DeclKind::This }]))?;
        let mut globals = Decls::default();
        globals.vars(&program.body);
        globals.lexical(&program.body);
        for d in &globals.0 {
            match d.kind {
                DeclKind::Function(f) => {
                    let n = self.function_declaration(f)?;
                    self.emit(Insn::CreateClosure(n));
                }
                DeclKind::Var => {
                    let k = self.name(d.name);
                    self.emit(Insn::LdaGlobalOrUndefined(k));
                }
                DeclKind::Const => {
                    self.global_consts.insert(d.name);
                    continue;
                }
                _ => continue,
            }
            let k = self.name(d.name);
            self.emit(Insn::StaGlobal(k));
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
    fn exit_plan(&self, target: Option<(Option<&'a str>, bool)>) -> (Exit, u16) {
        let f = self.frames.last().expect("inside a function");
        let mut handlers = 0;
        for (i, c) in f.controls.iter().enumerate().rev() {
            match c {
                Control::Handler => handlers += 1,
                Control::Finally(_) => return (Exit::Finally(i), handlers),
                Control::Jump(j) => {
                    let hit = match target {
                        Some((Some(l), false)) => j.label == Some(l),
                        Some((Some(l), true)) => j.label == Some(l) && j.continuable,
                        Some((None, false)) => j.breakable,
                        Some((None, true)) => j.continuable,
                        None => false,
                    };
                    if hit {
                        return (Exit::Jump(i), handlers);
                    }
                }
            }
        }
        (Exit::Function, handlers)
    }

    /// 退掉多进的环境与处理器，回到控制栈第 `i` 层（它进来时有 `envs` 层环境）。
    fn unwind_to(&mut self, envs: u16, handlers: u16) {
        for _ in envs..self.frame().envs {
            self.emit(Insn::PopEnv);
        }
        for _ in 0..handlers {
            self.emit(Insn::PopHandler);
        }
    }

    /// 跳进第 `i` 层的 finally，带上完成类型 `kind`。
    fn enter_finally(&mut self, i: usize, handlers: u16, kind: f64) {
        let Control::Finally(fin) = &self.frame().controls[i] else { unreachable!("a finally") };
        let (envs, kind_reg) = (fin.envs, fin.kind);
        self.unwind_to(envs, handlers);
        let k = self.constant(Constant::Number(kind));
        self.emit(Insn::LdaConst(k));
        self.emit(Insn::Star(kind_reg));
        let at = self.jump(Insn::Jump);
        let Control::Finally(fin) = &mut self.frame().controls[i] else { unreachable!("a finally") };
        fin.entries.push(at);
    }

    /// `break` / `continue`：退掉多进的环境与处理器再跳；中间隔着 finally 就先绕进它。
    fn exit(&mut self, span: Span, label: Option<&'a str>, cont: bool) -> Result<(), Error> {
        match self.exit_plan(Some((label, cont))) {
            (Exit::Jump(i), handlers) => {
                let Control::Jump(j) = &self.frame().controls[i] else { unreachable!("a jump target") };
                let envs = j.envs;
                self.unwind_to(envs, handlers);
                let at = self.jump(Insn::Jump);
                let Control::Jump(j) = &mut self.frame().controls[i] else { unreachable!("a jump target") };
                if cont { j.continues.push(at) } else { j.breaks.push(at) }
                Ok(())
            }
            (Exit::Finally(i), handlers) => {
                let Control::Finally(fin) = &mut self.frame().controls[i] else { unreachable!("a finally") };
                let n = fin.routes.len();
                fin.routes.push(Route { span, label, cont });
                self.enter_finally(i, handlers, COMPLETION_ROUTE + f64::from(u32::try_from(n).expect("fewer than 2^32 routes")));
                Ok(())
            }
            (Exit::Function, _) => Err(self.unsupported(span, "a jump with no target")),
        }
    }

    /// `return`（值在 acc）：中间隔着 finally 就先把值存好绕进它。
    fn ret(&mut self) {
        match self.exit_plan(None) {
            (Exit::Finally(i), handlers) => {
                let Control::Finally(fin) = &self.frame().controls[i] else { unreachable!("a finally") };
                let value = fin.value;
                self.emit(Insn::Star(value));
                self.enter_finally(i, handlers, COMPLETION_RETURN);
            }
            _ => _ = self.emit(Insn::Return),
        }
    }

    fn block(&mut self, b: &'a BlockStatement<'a>) -> Result<(), Error> {
        self.scoped(&Decls::block([b.body.as_slice()]), |c| c.statements(&b.body))
    }

    /// 按完成类型分派：是 `kind` 就做 `then`。
    fn on_completion(&mut self, reg: Reg, kind: f64, then: impl FnOnce(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
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
            Statement::FunctionDeclaration(_) | Statement::EmptyStatement(_) => {}
            Statement::ClassDeclaration(c) => {
                self.class(c, false)?;
                if let Some(id) = &c.id {
                    self.store(id.name.as_str(), true);
                }
            }
            Statement::BlockStatement(b) => self.block(b)?,
            Statement::ReturnStatement(r) => {
                match &r.argument {
                    Some(e) => self.expr(e)?,
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
                self.statement(&i.consequent)?;
                if let Some(a) = &i.alternate {
                    let end = self.jump(Insn::Jump);
                    self.land(skip);
                    self.statement(a)?;
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
                if matches!(l.body, Statement::WhileStatement(_) | Statement::DoWhileStatement(_) | Statement::ForStatement(_) | Statement::SwitchStatement(_)) {
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
                (Some(e), _) => self.expr(e)?,
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
