//! 作用域：每层声明了哪些名字（[`Decls`]），以及哪些绑定被内层函数捕获（[`Resolver`]）。
//!
//! 预扫一遍和生成代码用同一套 [`Decls`]，名字解析的结果才一致：被捕获的绑定放堆上的环境，其余放寄存器。
use std::collections::HashSet;

use oxc_ast::ast::{
    Argument, ArrayExpressionElement, ArrowFunctionBody, AssignmentTargetMaybeDefault, AssignmentTargetProperty, CatchClause, ChainElement, Class, ClassElement, ArrowFunctionExpression, AssignmentTarget, BindingIdentifier, BindingPattern, Expression, ForStatementInit, ForStatementLeft, FormalParameters, Function, MemberExpression, MethodDefinitionKind, ObjectPropertyKind, PrivateIdentifier, PropertyKey, SimpleAssignmentTarget, Statement, VariableDeclaration, VariableDeclarationKind,
};
use oxc_allocator::Allocator;
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
    /// `let` / `const` / `class`：声明在这个位置之后才初始化（暂时性死区）；其余没有
    pub ready: Option<u32>,
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
        Decl { name: id.name.as_str(), key: Self::key(id.span), kind, ready: None }
    }

    /// 带暂时性死区的绑定（`ready` 是声明结束的位置）。
    fn lexical_ident(id: &'a BindingIdentifier<'a>, kind: DeclKind<'a>, ready: u32) -> Decl<'a> {
        Decl { ready: Some(ready), ..Self::ident(id, kind) }
    }

    /// 模式里的全部绑定名（`ready`：暂时性死区到哪儿结束）。
    fn pattern(&mut self, p: &'a BindingPattern<'a>, kind: DeclKind<'a>, ready: Option<u32>) {
        match p {
            BindingPattern::BindingIdentifier(id) => self.add(Decl { ready, ..Self::ident(id, kind) }),
            BindingPattern::ObjectPattern(o) => {
                o.properties.iter().for_each(|b| self.pattern(&b.value, kind, ready));
                if let Some(r) = &o.rest {
                    self.pattern(&r.argument, kind, ready);
                }
            }
            BindingPattern::ArrayPattern(a) => {
                a.elements.iter().flatten().for_each(|e| self.pattern(e, kind, ready));
                if let Some(r) = &a.rest {
                    self.pattern(&r.argument, kind, ready);
                }
            }
            BindingPattern::AssignmentPattern(a) => self.pattern(&a.left, kind, ready),
        }
    }

    /// 变量声明里的绑定（含解构）。
    fn declarators(&mut self, d: &'a VariableDeclaration<'a>, kind: DeclKind<'a>) {
        let lexical = matches!(kind, DeclKind::Let | DeclKind::Const);
        for v in &d.declarations {
            self.pattern(&v.id, kind, lexical.then_some(v.span.end));
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
            Statement::ForInStatement(f) => {
                if let ForStatementLeft::VariableDeclaration(d) = &f.left
                    && d.kind == VariableDeclarationKind::Var
                {
                    self.declarators(d, DeclKind::Var);
                }
                self.var_in(&f.body);
            }
            Statement::ForOfStatement(f) => {
                if let ForStatementLeft::VariableDeclaration(d) = &f.left
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
                        self.add(Self::lexical_ident(id, DeclKind::Let, c.span.end));
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

    /// `for (let x of …)` / `for (const k in …)` 的头（每轮一份）。
    pub fn for_head(left: &'a ForStatementLeft<'a>) -> Self {
        let mut d = Self::default();
        if let ForStatementLeft::VariableDeclaration(v) = left {
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
        if let Some(p) = &c.param {
            d.pattern(&p.pattern, DeclKind::Let, None);
        }
        d
    }

    /// 类作用域：类自己的名字（类体里用，声明与表达式都有），加各个私有名、私有方法函数、计算字段键的隐藏绑定。
    pub fn class_scope(c: &'a Class<'a>, alloc: &'a Allocator) -> Self {
        let mut d = Self::default();
        if let Some(id) = &c.id {
            d.add(Self::lexical_ident(id, DeclKind::Const, c.span.end));
        }
        let hidden = |name, span: Span| Decl { name, key: Self::key(span), kind: DeclKind::Const, ready: None };
        for e in &c.body.body {
            match e {
                ClassElement::MethodDefinition(m) => {
                    if let PropertyKey::PrivateIdentifier(id) = &m.key {
                        d.add(hidden(Hidden::private(alloc, id), id.span));
                        d.add(hidden(Hidden::method(alloc, id, m.kind), m.value.span));
                    }
                }
                ClassElement::PropertyDefinition(p) => match &p.key {
                    PropertyKey::PrivateIdentifier(id) => d.add(hidden(Hidden::private(alloc, id), id.span)),
                    _ if p.computed => d.add(hidden(Hidden::field_key(alloc, p.span), p.span)),
                    _ => {}
                },
                _ => {}
            }
        }
        d
    }

    /// 静态块：自己一层 `var` 作用域，加顶层词法声明。
    pub fn static_block(stmts: &'a [Statement<'a>]) -> Self {
        let mut d = Self::default();
        d.vars(stmts);
        d.lexical(stmts);
        d
    }

    /// 函数体那一层：`this`、自己的名字、形参、`var`、顶层词法声明。
    pub fn function(this: Option<Key>, self_name: Option<&'a BindingIdentifier<'a>>, params: Option<&'a FormalParameters<'a>>, stmts: &'a [Statement<'a>]) -> Self {
        let mut d = Self::default();
        if let Some(key) = this {
            d.add(Decl { name: THIS, key, kind: DeclKind::This, ready: None });
        }
        if let Some(id) = self_name {
            d.add(Self::ident(id, DeclKind::SelfName));
        }
        for (i, p) in params.into_iter().flat_map(|p| p.items.iter()).enumerate() {
            match &p.pattern {
                BindingPattern::BindingIdentifier(id) => d.add(Self::ident(id, DeclKind::Param(u16::try_from(i).unwrap_or(u16::MAX)))),
                // 解构形参：寄存器里放原值，绑定另分配
                pat => d.pattern(pat, DeclKind::Var, None),
            }
        }
        if let Some(r) = params.and_then(|p| p.rest.as_ref()) {
            d.pattern(&r.rest.argument, DeclKind::Var, None);
        }
        d.vars(stmts);
        d.lexical(stmts);
        d
    }
}

/// 类作用域里的隐藏绑定名：私有名 `#x`、私有方法的函数 `#x@method` / `@get` / `@set`、计算字段的键 `%key@位置`。
/// 这些都不是合法标识符，不会和用户的名字撞。
pub(crate) struct Hidden;

impl Hidden {
    pub fn private<'a>(alloc: &'a Allocator, id: &PrivateIdentifier<'_>) -> &'a str {
        alloc.alloc_str(&format!("#{}", id.name.as_str()))
    }

    pub fn method<'a>(alloc: &'a Allocator, id: &PrivateIdentifier<'_>, kind: MethodDefinitionKind) -> &'a str {
        let suffix = match kind {
            MethodDefinitionKind::Get => "get",
            MethodDefinitionKind::Set => "set",
            MethodDefinitionKind::Method | MethodDefinitionKind::Constructor => "method",
        };
        alloc.alloc_str(&format!("#{}@{suffix}", id.name.as_str()))
    }

    pub fn field_key(alloc: &Allocator, span: Span) -> &str {
        alloc.alloc_str(&format!("%key@{}", span.start))
    }
}

/// 预扫的一层作用域：属于哪个函数、声明了什么。
struct Level<'a> {
    func: u32,
    names: Vec<(&'a str, Key)>,
}

/// 预扫：找出被内层函数引用到的绑定。
pub(crate) struct Resolver<'a> {
    levels: Vec<Level<'a>>,
    funcs: u32,
    func: u32,
    alloc: &'a Allocator,
    pub captured: HashSet<Key>,
}

impl<'a> Resolver<'a> {
    /// 脚本：顶层声明是全局对象的属性，不进作用域；只有 `this`。
    pub fn script(stmts: &'a [Statement<'a>], this: Key, alloc: &'a Allocator) -> HashSet<Key> {
        let mut r = Self { levels: Vec::new(), funcs: 0, func: 0, alloc, captured: HashSet::new() };
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

    /// 类：`extends`、类作用域、方法、计算键、字段初始化与静态块（各自一个合成函数）。
    fn class(&mut self, c: &'a Class<'a>) {
        if let Some(h) = &c.heritage {
            self.expr(&h.expression);
        }
        self.push(&Decls::class_scope(c, self.alloc));
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
                match e {
                    // 实例初始化函数里要装私有方法：读私有名与方法函数
                    ClassElement::MethodDefinition(m) if !statik && !m.r#static => {
                        if let PropertyKey::PrivateIdentifier(id) = &m.key {
                            self.reference(Hidden::private(self.alloc, id));
                            self.reference(Hidden::method(self.alloc, id, m.kind));
                        }
                    }
                    ClassElement::PropertyDefinition(p) if p.r#static == statik => {
                        match &p.key {
                            PropertyKey::PrivateIdentifier(id) => self.reference(Hidden::private(self.alloc, id)),
                            _ if p.computed => self.reference(Hidden::field_key(self.alloc, p.span)),
                            _ => {}
                        }
                        if let Some(v) = &p.value {
                            self.expr(v);
                        }
                    }
                    ClassElement::StaticBlock(b) if statik => self.scoped(&Decls::static_block(&b.body), |r| r.statements(&b.body)),
                    _ => {}
                }
            }
            self.leave_function(outer);
        }
        self.levels.pop();
    }

    fn private_reference(&mut self, id: &PrivateIdentifier<'_>) {
        let name = format!("#{}", id.name.as_str());
        self.reference(&name);
    }

    fn params(&mut self, p: &'a FormalParameters<'a>) {
        for i in &p.items {
            if let Some(e) = &i.initializer {
                self.expr(e);
            }
            self.pattern(&i.pattern);
        }
        if let Some(r) = &p.rest {
            self.pattern(&r.rest.argument);
        }
    }

    /// 模式里的默认值与计算键。
    fn pattern(&mut self, p: &'a BindingPattern<'a>) {
        match p {
            BindingPattern::BindingIdentifier(_) => {}
            BindingPattern::ObjectPattern(o) => {
                for b in &o.properties {
                    if b.computed
                        && let Some(k) = b.key.as_expression()
                    {
                        self.expr(k);
                    }
                    self.pattern(&b.value);
                }
                if let Some(r) = &o.rest {
                    self.pattern(&r.argument);
                }
            }
            BindingPattern::ArrayPattern(a) => {
                a.elements.iter().flatten().for_each(|e| self.pattern(e));
                if let Some(r) = &a.rest {
                    self.pattern(&r.argument);
                }
            }
            BindingPattern::AssignmentPattern(a) => {
                self.pattern(&a.left);
                self.expr(&a.right);
            }
        }
    }

    /// 赋值目标（含解构赋值）里引用到的名字、对象、键、默认值。
    fn target(&mut self, t: &'a AssignmentTarget<'a>) {
        match t {
            AssignmentTarget::ArrayAssignmentTarget(a) => {
                for e in a.elements.iter().flatten() {
                    self.maybe_default(e);
                }
                if let Some(r) = &a.rest {
                    self.target(&r.target);
                }
            }
            AssignmentTarget::ObjectAssignmentTarget(o) => {
                for p in &o.properties {
                    match p {
                        AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(i) => {
                            self.reference(i.binding.name.as_str());
                            if let Some(e) = &i.init {
                                self.expr(e);
                            }
                        }
                        AssignmentTargetProperty::AssignmentTargetPropertyProperty(p) => {
                            if p.computed
                                && let Some(k) = p.name.as_expression()
                            {
                                self.expr(k);
                            }
                            self.maybe_default(&p.binding);
                        }
                    }
                }
                if let Some(r) = &o.rest {
                    self.target(&r.target);
                }
            }
            t => {
                if let Some(s) = t.as_simple_assignment_target() {
                    self.simple_target(s);
                }
            }
        }
    }

    fn maybe_default(&mut self, e: &'a AssignmentTargetMaybeDefault<'a>) {
        match e {
            AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => {
                self.target(&d.binding);
                self.expr(&d.init);
            }
            t => {
                if let Some(t) = t.as_assignment_target() {
                    self.target(t);
                }
            }
        }
    }

    fn argument(&mut self, a: &'a Argument<'a>) {
        match a {
            Argument::SpreadElement(s) => self.expr(&s.argument),
            a => {
                if let Some(e) = a.as_expression() {
                    self.expr(e);
                }
            }
        }
    }

    fn chain(&mut self, c: &'a ChainElement<'a>) {
        match c {
            ChainElement::CallExpression(call) => {
                self.expr(&call.callee);
                call.arguments.iter().for_each(|a| self.argument(a));
            }
            c => {
                if let Some(m) = c.as_member_expression() {
                    match m {
                        MemberExpression::StaticMemberExpression(m) => self.expr(&m.object),
                        MemberExpression::ComputedMemberExpression(m) => {
                            self.expr(&m.object);
                            self.expr(&m.expression);
                        }
                        MemberExpression::PrivateFieldExpression(m) => {
                            self.private_reference(&m.field);
                            self.expr(&m.object);
                        }
                    }
                }
            }
        }
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
            Statement::ClassDeclaration(c) => self.class(c),
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
            Statement::ForInStatement(f) => self.for_each(&f.left, &f.right, &f.body),
            Statement::ForOfStatement(f) => self.for_each(&f.left, &f.right, &f.body),
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
                    self.scoped(&Decls::catch_param(c), |r| {
                        if let Some(p) = &c.param {
                            r.pattern(&p.pattern);
                        }
                        r.scoped(&Decls::block([c.body.body.as_slice()]), |r| r.statements(&c.body.body));
                    });
                }
                if let Some(f) = &t.finalizer {
                    self.scoped(&Decls::block([f.body.as_slice()]), |r| r.statements(&f.body));
                }
            }
            _ => {}
        }
    }

    /// `for-in` / `for-of`：右边在外面求，左边与循环体在每轮的那层里。
    fn for_each(&mut self, left: &'a ForStatementLeft<'a>, right: &'a Expression<'a>, body: &'a Statement<'a>) {
        self.expr(right);
        self.scoped(&Decls::for_head(left), |r| {
            match left {
                ForStatementLeft::VariableDeclaration(d) => r.declaration(d),
                t => {
                    if let Some(t) = t.as_assignment_target() {
                        r.target(t);
                    }
                }
            }
            r.statement(body);
        });
    }

    fn declaration(&mut self, d: &'a VariableDeclaration<'a>) {
        for v in &d.declarations {
            self.pattern(&v.id);
            if let Some(e) = &v.init {
                self.expr(e);
            }
        }
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
                    Some(MemberExpression::PrivateFieldExpression(m)) => {
                        self.private_reference(&m.field);
                        self.expr(&m.object);
                    }
                    None => {}
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
            Expression::ClassExpression(c) => self.class(c),
            Expression::NewExpression(n) => {
                self.expr(&n.callee);
                n.arguments.iter().for_each(|a| self.argument(a));
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
                self.target(&a.left);
                self.expr(&a.right);
            }
            Expression::ChainExpression(c) => self.chain(&c.expression),
            Expression::YieldExpression(y) => {
                if let Some(a) = &y.argument {
                    self.expr(a);
                }
            }
            Expression::AwaitExpression(a) => self.expr(&a.argument),
            Expression::UpdateExpression(u) => self.simple_target(&u.argument),
            Expression::CallExpression(c) => {
                self.expr(&c.callee);
                c.arguments.iter().for_each(|a| self.argument(a));
            }
            Expression::StaticMemberExpression(m) => self.expr(&m.object),
            Expression::PrivateFieldExpression(m) => {
                self.private_reference(&m.field);
                self.expr(&m.object);
            }
            Expression::PrivateInExpression(p) => {
                self.private_reference(&p.left);
                self.expr(&p.right);
            }
            Expression::ComputedMemberExpression(m) => {
                self.expr(&m.object);
                self.expr(&m.expression);
            }
            Expression::ObjectExpression(o) => {
                for p in &o.properties {
                    match p {
                        ObjectPropertyKind::ObjectProperty(p) => {
                            if let Some(k) = p.key.as_expression() {
                                self.expr(k);
                            }
                            self.expr(&p.value);
                        }
                        ObjectPropertyKind::SpreadProperty(s) => self.expr(&s.argument),
                    }
                }
            }
            Expression::ArrayExpression(a) => {
                for el in &a.elements {
                    match el {
                        ArrayExpressionElement::SpreadElement(s) => self.expr(&s.argument),
                        el => {
                            if let Some(e) = el.as_expression() {
                                self.expr(e);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
