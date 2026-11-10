//! JS ⇒ 字节码：作用域与函数、语句。表达式在 `expr.rs`。
use std::collections::HashSet;

use oxc_allocator::Allocator;
use oxc_ast::ast::{BindingIdentifier, BindingPattern, Expression, ForStatementInit, FormalParameters, Function as AstFunction, Program, Statement, SwitchStatement, VariableDeclaration, VariableDeclarationKind};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};
use r8frame::{CompareOp, Error, Source};

use crate::scope::{Decl, DeclKind, Decls, Key, Resolver, THIS};
use crate::{Const, Constant, Function, Insn, Label, Module, Reg};

/// 回填前的跳转目标
const UNPATCHED: Label = Label(u32::MAX);

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

/// 正在编的一个函数。
struct Frame<'a> {
    index: usize,
    out: Function,
    /// 下一个空寄存器
    top: u16,
    /// 当前有几层本函数的环境
    envs: u16,
    jumps: Vec<Jump<'a>>,
    /// 下一个循环 / `switch` 的标签
    label: Option<&'a str>,
    /// 脚本的完成值放这里（只有脚本本身有）
    completion: Option<Reg>,
}

/// 函数体：语句，或箭头函数的表达式。
#[derive(Clone, Copy)]
pub(crate) enum Body<'a> {
    Statements(&'a [Statement<'a>]),
    Expression(&'a Expression<'a>),
}

pub(crate) struct Compiler<'a, 's> {
    src: &'s Source<'s>,
    captured: HashSet<Key>,
    module: Vec<Function>,
    frames: Vec<Frame<'a>>,
    levels: Vec<Level<'a>>,
    /// 脚本顶层的 `const`（是全局对象的属性，但不许再赋值）
    global_consts: HashSet<&'a str>,
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
        let mut c = Compiler { src, captured, module: Vec::new(), frames: Vec::new(), levels: Vec::new(), global_consts: HashSet::new() };
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
    pub(crate) fn function(&mut self, name: &str, this: Option<Key>, self_name: Option<&'a BindingIdentifier<'a>>, params: &'a FormalParameters<'a>, body: Body<'a>) -> Result<u32, Error> {
        if let Some(r) = &params.rest {
            return Err(self.unsupported(r.span, "rest parameters"));
        }
        for p in &params.items {
            if !matches!(p.pattern, BindingPattern::BindingIdentifier(_)) {
                return Err(self.unsupported(p.span, "destructuring parameters"));
            }
            if p.initializer.is_some() {
                return Err(self.unsupported(p.span, "default parameters"));
            }
        }
        let n = u16::try_from(params.items.len()).map_err(|_| self.unsupported(params.span, "more than 65535 parameters"))?;
        let index = self.module.len();
        self.module.push(Function::default());
        let top = Reg::PARAMS + n;
        let out = Function { name: name.into(), params: n, regs: top, ..Function::default() };
        self.frames.push(Frame { index, out, top, envs: 0, jumps: Vec::new(), label: None, completion: None });
        let stmts = match body {
            Body::Statements(s) => s,
            Body::Expression(_) => &[],
        };
        self.enter(&Decls::function(this, self_name, params, stmts))?;
        match body {
            Body::Statements(s) => self.statements(s)?,
            Body::Expression(e) => {
                self.expr(e)?;
                self.emit(Insn::Return);
            }
        }
        self.emit(Insn::LdaUndefined);
        self.emit(Insn::Return);
        self.levels.pop();
        let f = self.frames.pop().expect("the function's frame");
        self.module[f.index] = f.out;
        Ok(u32::try_from(index).expect("fewer than 2^32 functions"))
    }

    /// 普通函数（声明或表达式）：查掉不支持的种类。
    pub(crate) fn plain_function(&mut self, f: &'a AstFunction<'a>, self_name: bool) -> Result<u32, Error> {
        if f.generator || f.r#async {
            return Err(self.unsupported(f.span, "generators / async functions"));
        }
        let body = f.body.as_ref().ok_or_else(|| self.unsupported(f.span, "a function without a body"))?;
        let name = f.id.as_ref().map_or("", |id| id.name.as_str());
        self.function(name, Some(Decls::this_key(f.span)), if self_name { f.id.as_ref() } else { None }, &f.params, Body::Statements(&body.statements))
    }

    fn function_declaration(&mut self, f: &'a AstFunction<'a>) -> Result<u32, Error> {
        self.plain_function(f, false)
    }

    /// 脚本本身：顶层 `var` / 函数声明 / `let` / `const` 都是全局对象的属性。
    fn script(&mut self, program: &'a Program<'a>) -> Result<(), Error> {
        self.module.push(Function::default());
        let out = Function { name: "main".into(), regs: Reg::PARAMS, ..Function::default() };
        self.frames.push(Frame { index: 0, out, top: Reg::PARAMS, envs: 0, jumps: Vec::new(), label: None, completion: None });
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
        f.jumps.push(Jump { label, breakable, continuable, breaks: Vec::new(), continues: Vec::new(), envs });
    }

    fn pop_jump(&mut self, brk: Label, cont: Option<Label>) {
        let j = self.frame().jumps.pop().expect("a jump target");
        j.breaks.into_iter().for_each(|at| self.patch(at, brk));
        if let Some(c) = cont {
            j.continues.into_iter().for_each(|at| self.patch(at, c));
        }
    }

    /// 循环 / `switch` 用掉挂着的标签。
    fn take_label(&mut self) -> Option<&'a str> {
        self.frame().label.take()
    }

    /// `break` / `continue`：退掉多进的环境再跳。
    fn exit(&mut self, span: Span, label: Option<&'a str>, cont: bool) -> Result<(), Error> {
        let f = self.frames.last().expect("inside a function");
        let i = f
            .jumps
            .iter()
            .rposition(|j| match (label, cont) {
                (Some(l), false) => j.label == Some(l),
                (Some(l), true) => j.label == Some(l) && j.continuable,
                (None, false) => j.breakable,
                (None, true) => j.continuable,
            })
            .ok_or_else(|| self.unsupported(span, "a jump with no target"))?;
        for _ in f.jumps[i].envs..f.envs {
            self.emit(Insn::PopEnv);
        }
        let at = self.jump(Insn::Jump);
        let j = &mut self.frame().jumps[i];
        if cont { j.continues.push(at) } else { j.breaks.push(at) }
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
            Statement::BlockStatement(b) => self.scoped(&Decls::block([b.body.as_slice()]), |c| c.statements(&b.body))?,
            Statement::ReturnStatement(r) => {
                match &r.argument {
                    Some(e) => self.expr(e)?,
                    None => _ = self.emit(Insn::LdaUndefined),
                }
                self.emit(Insn::Return);
            }
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
                return Err(self.unsupported(v.span, "destructuring"));
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
