//! 作用域：每层声明了哪些名字（[`Decls`]），以及哪些绑定被内层函数捕获（[`Resolver`]）。
//!
//! 预扫一遍和生成代码用同一套 [`Decls`]，名字解析的结果才一致：被捕获的绑定放堆上的环境，其余放寄存器。
use std::collections::HashSet;

use oxc_ast::ast::{
    Argument, ArrayExpressionElement, ArrowFunctionBody, CatchClause, Class, ClassElement, ArrowFunctionExpression, AssignmentTarget, BindingIdentifier, BindingPattern, Expression, ForStatementInit, FormalParameters, Function, MemberExpression, ObjectPropertyKind, SimpleAssignmentTarget, Statement, VariableDeclaration, VariableDeclarationKind,
};
use oxc_span::Span;

/// 一个绑定的身份：声明处的位置（`this` 另占一个名字空间）。
pub(crate) type Key = u64;

/// `this` 的键与普通绑定的键分开
const THIS_KEY: Key = 1 << 32;

/// `this` 这个名字不可能是标识符，拿来当伪绑定名
pub(crate) const THIS: &str = "this";

/// 一个声明的种类。
#[derive(Clone, Copy)]
pub(crate) enum DeclKind<'a> {
    /// 非箭头函数的 `this`（`r0`）
    This,
    /// 具名函数表达式里自己的名字（`r1`）
    SelfName,
    /// 第 n 个形参
    Param(u16),
    Var,
    Let,
    Const,
    /// 函数声明：进作用域时建闭包
    Function(&'a Function<'a>),
}

#[derive(Clone, Copy)]
pub(crate) struct Decl<'a> {
    pub name: &'a str,
    pub key: Key,
    pub kind: DeclKind<'a>,
}

/// 一层作用域声明的名字，同名只留一个（后来的函数声明覆盖前面的）。
#[derive(Default)]
pub(crate) struct Decls<'a>(pub Vec<Decl<'a>>);

impl<'a> Decls<'a> {
    pub fn key(span: Span) -> Key {
        Key::from(span.start)
    }

    pub fn this_key(span: Span) -> Key {
        THIS_KEY | Key::from(span.start)
    }

    fn add(&mut self, d: Decl<'a>) {
        match self.0.iter_mut().find(|e| e.name == d.name) {
            Some(e) if matches!(d.kind, DeclKind::Function(_)) => e.kind = d.kind,
            Some(_) => {}
            None => self.0.push(d),
        }
    }

    fn ident(id: &'a BindingIdentifier<'a>, kind: DeclKind<'a>) -> Decl<'a> {
        Decl { name: id.name.as_str(), key: Self::key(id.span), kind }
    }

    /// 变量声明里的简单绑定（解构在生成代码时报错，这里跳过）。
    fn declarators(&mut self, d: &'a VariableDeclaration<'a>, kind: DeclKind<'a>) {
        for v in &d.declarations {
            if let BindingPattern::BindingIdentifier(id) = &v.id {
                self.add(Self::ident(id, kind));
            }
        }
    }

    /// `var`：穿过块、不进函数。
    pub fn vars(&mut self, stmts: &'a [Statement<'a>]) {
        for s in stmts {
            self.var_in(s);
        }
    }

    fn var_in(&mut self, s: &'a Statement<'a>) {
        match s {
            Statement::VariableDeclaration(d) if d.kind == VariableDeclarationKind::Var => self.declarators(d, DeclKind::Var),
            Statement::BlockStatement(b) => self.vars(&b.body),
            Statement::IfStatement(i) => {
                self.var_in(&i.consequent);
                if let Some(a) = &i.alternate {
                    self.var_in(a);
                }
            }
            Statement::WhileStatement(w) => self.var_in(&w.body),
            Statement::DoWhileStatement(w) => self.var_in(&w.body),
            Statement::ForStatement(f) => {
                if let Some(ForStatementInit::VariableDeclaration(d)) = &f.init
                    && d.kind == VariableDeclarationKind::Var
                {
                    self.declarators(d, DeclKind::Var);
                }
                self.var_in(&f.body);
            }
            Statement::LabeledStatement(l) => self.var_in(&l.body),
            Statement::SwitchStatement(sw) => sw.cases.iter().for_each(|c| self.vars(&c.consequent)),
            _ => {}
        }
    }

    /// 词法声明（`let` / `const` / 函数声明），只看这一层。
    pub fn lexical(&mut self, stmts: &'a [Statement<'a>]) {
        for s in stmts {
            match s {
                Statement::VariableDeclaration(d) if d.kind == VariableDeclarationKind::Let => self.declarators(d, DeclKind::Let),
                Statement::VariableDeclaration(d) if d.kind == VariableDeclarationKind::Const => self.declarators(d, DeclKind::Const),
                Statement::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        self.add(Self::ident(id, DeclKind::Function(f)));
                    }
                }
                Statement::ClassDeclaration(c) => {
                    if let Some(id) = &c.id {
                        self.add(Self::ident(id, DeclKind::Let));
                    }
                }
                _ => {}
            }
        }
    }

    /// 一个块（或 `switch` 的全部分支）。
    pub fn block(stmts: impl IntoIterator<Item = &'a [Statement<'a>]>) -> Self {
        let mut d = Self::default();
        stmts.into_iter().for_each(|s| d.lexical(s));
        d
    }

    /// `for (let …; …)` 的头。
    pub fn for_init(init: Option<&'a ForStatementInit<'a>>) -> Self {
        let mut d = Self::default();
        if let Some(ForStatementInit::VariableDeclaration(v)) = init {
            match v.kind {
                VariableDeclarationKind::Let => d.declarators(v, DeclKind::Let),
                VariableDeclarationKind::Const => d.declarators(v, DeclKind::Const),
                _ => {}
            }
        }
        d
    }

    /// `catch (e)` 的形参那一层。
    pub fn catch_param(c: &'a CatchClause<'a>) -> Self {
        let mut d = Self::default();
        if let Some(BindingPattern::BindingIdentifier(id)) = c.param.as_ref().map(|p| &p.pattern) {
            d.add(Self::ident(id, DeclKind::Let));
        }
        d
    }

    /// 函数体那一层：`this`、自己的名字、形参、`var`、顶层词法声明。
    /// 具名类表达式里类自己的名字那一层。
    pub fn class_name(c: &'a Class<'a>) -> Self {
        let mut d = Self::default();
        if let Some(id) = &c.id {
            d.add(Self::ident(id, DeclKind::Const));
        }
        d
    }

    pub fn function(this: Option<Key>, self_name: Option<&'a BindingIdentifier<'a>>, params: Option<&'a FormalParameters<'a>>, stmts: &'a [Statement<'a>]) -> Self {
        let mut d = Self::default();
        if let Some(key) = this {
            d.add(Decl { name: THIS, key, kind: DeclKind::This });
        }
        if let Some(id) = self_name {
            d.add(Self::ident(id, DeclKind::SelfName));
        }
        for (i, p) in params.into_iter().flat_map(|p| p.items.iter()).enumerate() {
            if let BindingPattern::BindingIdentifier(id) = &p.pattern {
                d.add(Self::ident(id, DeclKind::Param(u16::try_from(i).unwrap_or(u16::MAX))));
            }
        }
        d.vars(stmts);
        d.lexical(stmts);
        d
    }
}

/// 预扫的一层作用域：属于哪个函数、声明了什么。
struct Level<'a> {
    func: u32,
    names: Vec<(&'a str, Key)>,
}

/// 预扫：找出被内层函数引用到的绑定。
#[derive(Default)]
pub(crate) struct Resolver<'a> {
    levels: Vec<Level<'a>>,
    funcs: u32,
    func: u32,
    pub captured: HashSet<Key>,
}

impl<'a> Resolver<'a> {
    /// 脚本：顶层声明是全局对象的属性，不进作用域；只有 `this`。
    pub fn script(stmts: &'a [Statement<'a>], this: Key) -> HashSet<Key> {
        let mut r = Self::default();
        r.levels.push(Level { func: 0, names: vec![(THIS, this)] });
        r.statements(stmts);
        r.captured
    }

    fn push(&mut self, d: &Decls<'a>) {
        self.levels.push(Level { func: self.func, names: d.0.iter().map(|d| (d.name, d.key)).collect() });
    }

    fn reference(&mut self, name: &str) {
        for l in self.levels.iter().rev() {
            if let Some(&(_, key)) = l.names.iter().find(|(n, _)| *n == name) {
                if l.func != self.func {
                    self.captured.insert(key);
                }
                return;
            }
        }
    }

    fn enter_function(&mut self, d: &Decls<'a>) -> u32 {
        self.funcs += 1;
        let outer = self.func;
        self.func = self.funcs;
        self.push(d);
        outer
    }

    fn leave_function(&mut self, outer: u32) {
        self.levels.pop();
        self.func = outer;
    }

    fn function(&mut self, f: &'a Function<'a>, self_name: bool) {
        let Some(body) = &f.body else { return };
        let d = Decls::function(Some(Decls::this_key(f.span)), if self_name { f.id.as_ref() } else { None }, Some(&f.params), &body.statements);
        let outer = self.enter_function(&d);
        self.params(&f.params);
        self.statements(&body.statements);
        self.leave_function(outer);
    }

    fn arrow(&mut self, a: &'a ArrowFunctionExpression<'a>) {
        let stmts: &'a [Statement<'a>] = match &a.body {
            ArrowFunctionBody::FunctionBody(b) => &b.statements,
            _ => &[],
        };
        let d = Decls::function(None, None, Some(&a.params), stmts);
        let outer = self.enter_function(&d);
        self.params(&a.params);
        match &a.body {
            ArrowFunctionBody::FunctionBody(_) => self.statements(stmts),
            e => {
                if let Some(e) = e.as_expression() {
                    self.expr(e);
                }
            }
        }
        self.leave_function(outer);
    }

    /// 类：`extends`、类名那一层、方法、计算键、字段初始化（各自一个合成函数）。
    fn class(&mut self, c: &'a Class<'a>, expression: bool) {
        if let Some(h) = &c.heritage {
            self.expr(&h.expression);
        }
        let named = expression && c.id.is_some();
        if named {
            self.push(&Decls::class_name(c));
        }
        for e in &c.body.body {
            match e {
                ClassElement::MethodDefinition(m) => {
                    if m.computed
                        && let Some(k) = m.key.as_expression()
                    {
                        self.expr(k);
                    }
                    self.function(&m.value, false);
                }
                ClassElement::PropertyDefinition(p) if p.computed => {
                    if let Some(k) = p.key.as_expression() {
                        self.expr(k);
                    }
                }
                _ => {}
            }
        }
        for (statik, key) in [(false, Decls::this_key(c.body.span)), (true, Decls::this_key(c.span))] {
            let outer = self.enter_function(&Decls::function(Some(key), None, None, &[]));
            for e in &c.body.body {
                if let ClassElement::PropertyDefinition(p) = e
                    && p.r#static == statik
                    && let Some(v) = &p.value
                {
                    self.expr(v);
                }
            }
            self.leave_function(outer);
        }
        if named {
            self.levels.pop();
        }
    }

    fn params(&mut self, p: &'a FormalParameters<'a>) {
        p.items.iter().filter_map(|i| i.initializer.as_deref()).for_each(|e| self.expr(e));
    }

    fn statements(&mut self, stmts: &'a [Statement<'a>]) {
        for s in stmts {
            self.statement(s);
        }
    }

    fn scoped(&mut self, d: &Decls<'a>, f: impl FnOnce(&mut Self)) {
        self.push(d);
        f(self);
        self.levels.pop();
    }

    fn statement(&mut self, s: &'a Statement<'a>) {
        match s {
            Statement::ExpressionStatement(e) => self.expr(&e.expression),
            Statement::VariableDeclaration(d) => self.declaration(d),
            Statement::FunctionDeclaration(f) => self.function(f, false),
            Statement::ClassDeclaration(c) => self.class(c, false),
            Statement::BlockStatement(b) => self.scoped(&Decls::block([b.body.as_slice()]), |r| r.statements(&b.body)),
            Statement::IfStatement(i) => {
                self.expr(&i.test);
                self.statement(&i.consequent);
                if let Some(a) = &i.alternate {
                    self.statement(a);
                }
            }
            Statement::WhileStatement(w) => {
                self.expr(&w.test);
                self.statement(&w.body);
            }
            Statement::DoWhileStatement(w) => {
                self.statement(&w.body);
                self.expr(&w.test);
            }
            Statement::ForStatement(f) => self.scoped(&Decls::for_init(f.init.as_ref()), |r| {
                match &f.init {
                    Some(ForStatementInit::VariableDeclaration(d)) => r.declaration(d),
                    Some(init) => {
                        if let Some(e) = init.as_expression() {
                            r.expr(e);
                        }
                    }
                    None => {}
                }
                f.test.iter().chain(&f.update).for_each(|e| r.expr(e));
                r.statement(&f.body);
            }),
            Statement::LabeledStatement(l) => self.statement(&l.body),
            Statement::SwitchStatement(sw) => {
                self.expr(&sw.discriminant);
                self.scoped(&Decls::block(sw.cases.iter().map(|c| c.consequent.as_slice())), |r| {
                    for c in &sw.cases {
                        if let Some(t) = &c.test {
                            r.expr(t);
                        }
                        r.statements(&c.consequent);
                    }
                });
            }
            Statement::ReturnStatement(r) => {
                if let Some(e) = &r.argument {
                    self.expr(e);
                }
            }
            Statement::ThrowStatement(t) => self.expr(&t.argument),
            Statement::TryStatement(t) => {
                self.scoped(&Decls::block([t.block.body.as_slice()]), |r| r.statements(&t.block.body));
                if let Some(c) = &t.handler {
                    self.scoped(&Decls::catch_param(c), |r| r.scoped(&Decls::block([c.body.body.as_slice()]), |r| r.statements(&c.body.body)));
                }
                if let Some(f) = &t.finalizer {
                    self.scoped(&Decls::block([f.body.as_slice()]), |r| r.statements(&f.body));
                }
            }
            _ => {}
        }
    }

    fn declaration(&mut self, d: &'a VariableDeclaration<'a>) {
        d.declarations.iter().filter_map(|v| v.init.as_ref()).for_each(|e| self.expr(e));
    }

    fn simple_target(&mut self, t: &'a SimpleAssignmentTarget<'a>) {
        match t {
            SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => self.reference(id.name.as_str()),
            t => {
                match t.as_member_expression() {
                    Some(MemberExpression::StaticMemberExpression(m)) => self.expr(&m.object),
                    Some(MemberExpression::ComputedMemberExpression(m)) => {
                        self.expr(&m.object);
                        self.expr(&m.expression);
                    }
                    _ => {}
                }
            }
        }
    }

    fn expr(&mut self, e: &'a Expression<'a>) {
        match e {
            Expression::Identifier(id) => self.reference(id.name.as_str()),
            // `super.x` / `super(...)` 都要读写 `this`
            Expression::ThisExpression(_) | Expression::Super(_) => self.reference(THIS),
            Expression::FunctionExpression(f) => self.function(f, true),
            Expression::ArrowFunctionExpression(a) => self.arrow(a),
            Expression::ClassExpression(c) => self.class(c, true),
            Expression::NewExpression(n) => {
                self.expr(&n.callee);
                n.arguments.iter().filter_map(Argument::as_expression).for_each(|e| self.expr(e));
            }
            Expression::ParenthesizedExpression(p) => self.expr(&p.expression),
            Expression::UnaryExpression(u) => self.expr(&u.argument),
            Expression::BinaryExpression(b) => {
                self.expr(&b.left);
                self.expr(&b.right);
            }
            Expression::LogicalExpression(b) => {
                self.expr(&b.left);
                self.expr(&b.right);
            }
            Expression::ConditionalExpression(c) => {
                self.expr(&c.test);
                self.expr(&c.consequent);
                self.expr(&c.alternate);
            }
            Expression::SequenceExpression(s) => s.expressions.iter().for_each(|e| self.expr(e)),
            Expression::TemplateLiteral(t) => t.expressions.iter().for_each(|e| self.expr(e)),
            Expression::AssignmentExpression(a) => {
                match &a.left {
                    AssignmentTarget::AssignmentTargetIdentifier(id) => self.reference(id.name.as_str()),
                    t => {
                        if let Some(s) = t.as_simple_assignment_target() {
                            self.simple_target(s);
                        }
                    }
                }
                self.expr(&a.right);
            }
            Expression::UpdateExpression(u) => self.simple_target(&u.argument),
            Expression::CallExpression(c) => {
                self.expr(&c.callee);
                c.arguments.iter().filter_map(Argument::as_expression).for_each(|e| self.expr(e));
            }
            Expression::StaticMemberExpression(m) => self.expr(&m.object),
            Expression::ComputedMemberExpression(m) => {
                self.expr(&m.object);
                self.expr(&m.expression);
            }
            Expression::ObjectExpression(o) => {
                for p in &o.properties {
                    if let ObjectPropertyKind::ObjectProperty(p) = p {
                        if let Some(k) = p.key.as_expression() {
                            self.expr(k);
                        }
                        self.expr(&p.value);
                    }
                }
            }
            Expression::ArrayExpression(a) => a.elements.iter().filter_map(ArrayExpressionElement::as_expression).for_each(|e| self.expr(e)),
            _ => {}
        }
    }
}
