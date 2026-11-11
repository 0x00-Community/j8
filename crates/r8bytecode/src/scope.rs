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

/// `new.target` 伪绑定的键空间
const NEW_TARGET_KEY: Key = 2 << 32;

/// `arguments` 伪绑定的键空间
const ARGUMENTS_KEY: Key = 3 << 32;

/// 键里的源码位置部分
const POSITION_MASK: Key = (1 << 32) - 1;

/// annexB 块级函数在外层建的 `var` 绑定：键 = 这个标记 | 函数名的位置
const ANNEXB_KEY: Key = 4 << 32;

/// `this` 这个名字不可能是标识符，拿来当伪绑定名
pub(crate) const THIS: &str = "this";

/// `new.target` 也不是标识符
pub(crate) const NEW_TARGET: &str = "new.target";

/// 非箭头函数里没被别的声明遮住的 `arguments`
pub(crate) const ARGUMENTS: &str = "arguments";

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
    /// 非箭头函数的 `new.target`（用到才在入口取）
    NewTarget,
    /// 非箭头函数的 `arguments` 对象（用到才在入口建）
    Arguments,
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

    /// annexB B.3.2：可以在外层再建一个 `var` 的块级普通函数声明（换成 `var F` 不会和沿途的词法声明、
    /// 形参冲突）。`outer` 是函数顶层的形参与词法名字。
    pub fn annexb(stmts: &'a [Statement<'a>], outer: Vec<&'a str>) -> Vec<&'a Function<'a>> {
        let mut out = Vec::new();
        let mut stack = alloc::vec![outer];
        for s in stmts {
            Self::annexb_stmt(s, &mut stack, &mut out);
        }
        out
    }

    /// 非严格函数：annexB 块级函数在函数层再加的 `var`（已经有同名的就不加）。
    fn annexb_vars(&mut self, stmts: &'a [Statement<'a>], params: Option<&'a FormalParameters<'a>>) {
        for f in Self::annexb_functions(stmts, params) {
            if let (Some(id), Some(key)) = (&f.id, Self::annexb_key(f)) {
                self.add(Decl { name: id.name.as_str(), key, kind: DeclKind::Var, ready: None });
            }
        }
    }

    /// 函数体（或脚本，`params` 为空）里能按 annexB 提升的块级函数（`arguments` 除外）。
    pub fn annexb_functions(stmts: &'a [Statement<'a>], params: Option<&'a FormalParameters<'a>>) -> Vec<&'a Function<'a>> {
        let mut top = Self::default();
        top.lexical(stmts);
        let mut outer: Vec<&'a str> = top.0.iter().filter(|d| !matches!(d.kind, DeclKind::Function(_))).map(|d| d.name).collect();
        let mut ps = Self::default();
        for p in params.into_iter().flat_map(|p| p.items.iter()) {
            ps.pattern(&p.pattern, DeclKind::Var, None);
        }
        if let Some(r) = params.and_then(|p| p.rest.as_ref()) {
            ps.pattern(&r.rest.argument, DeclKind::Var, None);
        }
        outer.extend(ps.0.iter().map(|d| d.name));
        let mut found = Self::annexb(stmts, outer);
        found.retain(|f| f.id.as_ref().is_some_and(|id| id.name != ARGUMENTS));
        found
    }

    /// 脚本：annexB 块级函数的名字当全局 `var` 声明。
    pub fn annexb_globals(&mut self, hoisted: &[&'a Function<'a>]) {
        for f in hoisted {
            if let (Some(id), Some(key)) = (&f.id, Self::annexb_key(f)) {
                self.add(Decl { name: id.name.as_str(), key, kind: DeclKind::Var, ready: None });
            }
        }
    }

    /// annexB 的 `var` 绑定的键。
    pub fn annexb_key(f: &Function<'_>) -> Option<Key> {
        f.id.as_ref().map(|id| ANNEXB_KEY | Self::key(id.span))
    }

    fn annexb_stmt(s: &'a Statement<'a>, stack: &mut Vec<Vec<&'a str>>, out: &mut Vec<&'a Function<'a>>) {
        match s {
            Statement::BlockStatement(b) => Self::annexb_block(&[b.body.as_slice()], stack, out),
            Statement::IfStatement(i) => {
                for clause in core::iter::once(&i.consequent).chain(i.alternate.as_ref()) {
                    if matches!(clause, Statement::FunctionDeclaration(_)) {
                        Self::annexb_block(&[core::slice::from_ref(clause)], stack, out);
                    } else {
                        Self::annexb_stmt(clause, stack, out);
                    }
                }
            }
            Statement::WhileStatement(w) => Self::annexb_stmt(&w.body, stack, out),
            Statement::DoWhileStatement(w) => Self::annexb_stmt(&w.body, stack, out),
            Statement::ForStatement(f) => {
                let mut head = Self::default();
                if let Some(ForStatementInit::VariableDeclaration(d)) = &f.init
                    && d.kind != VariableDeclarationKind::Var
                {
                    head.declarators(d, DeclKind::Let);
                }
                stack.push(head.0.iter().map(|d| d.name).collect());
                Self::annexb_stmt(&f.body, stack, out);
                stack.pop();
            }
            Statement::ForInStatement(f) => Self::annexb_loop(&f.left, &f.body, stack, out),
            Statement::ForOfStatement(f) => Self::annexb_loop(&f.left, &f.body, stack, out),
            Statement::LabeledStatement(l) => Self::annexb_stmt(&l.body, stack, out),
            Statement::SwitchStatement(sw) => {
                let cases: Vec<&'a [Statement<'a>]> = sw.cases.iter().map(|c| c.consequent.as_slice()).collect();
                Self::annexb_block(&cases, stack, out);
            }
            Statement::TryStatement(t) => {
                Self::annexb_block(&[t.block.body.as_slice()], stack, out);
                if let Some(h) = &t.handler {
                    // 简单的 catch 参数可以被 var 重新声明（B.3.4），解构的不行
                    let mut names = Self::default();
                    if let Some(p) = &h.param
                        && !matches!(p.pattern, BindingPattern::BindingIdentifier(_))
                    {
                        names.pattern(&p.pattern, DeclKind::Let, None);
                    }
                    stack.push(names.0.iter().map(|d| d.name).collect());
                    Self::annexb_block(&[h.body.body.as_slice()], stack, out);
                    stack.pop();
                }
                if let Some(f) = &t.finalizer {
                    Self::annexb_block(&[f.body.as_slice()], stack, out);
                }
            }
            _ => {}
        }
    }

    fn annexb_loop(left: &'a ForStatementLeft<'a>, body: &'a Statement<'a>, stack: &mut Vec<Vec<&'a str>>, out: &mut Vec<&'a Function<'a>>) {
        let mut head = Self::default();
        if let ForStatementLeft::VariableDeclaration(d) = left
            && d.kind != VariableDeclarationKind::Var
        {
            head.declarators(d, DeclKind::Let);
        }
        stack.push(head.0.iter().map(|d| d.name).collect());
        Self::annexb_stmt(body, stack, out);
        stack.pop();
    }

    fn annexb_block(parts: &[&'a [Statement<'a>]], stack: &mut Vec<Vec<&'a str>>, out: &mut Vec<&'a Function<'a>>) {
        let mut lexical = Self::default();
        for p in parts {
            lexical.lexical(p);
        }
        for f in parts.iter().flat_map(|p| p.iter()).filter_map(|s| match s {
            Statement::FunctionDeclaration(f) if !f.generator && !f.r#async => Some(f),
            _ => None,
        }) {
            let Some(id) = &f.id else { continue };
            let name = id.name.as_str();
            let other_lexical = lexical.0.iter().any(|d| d.name == name && !matches!(d.kind, DeclKind::Function(_)));
            if !other_lexical && !stack.iter().flatten().any(|n| *n == name) {
                out.push(f);
            }
        }
        stack.push(lexical.0.iter().map(|d| d.name).collect());
        for s in parts.iter().flat_map(|p| p.iter()) {
            Self::annexb_stmt(s, stack, out);
        }
        stack.pop();
    }

    /// `with` 体外面那一层：只有 with 对象的隐藏绑定。
    pub fn with_scope(name: &'a str, span: Span) -> Self {
        Self(alloc::vec![Decl { name, key: Self::key(span), kind: DeclKind::Let, ready: None }])
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

    /// eval 代码顶层的 `let` / `const` / `class`（函数声明仍是全局的）。
    pub fn eval_lexical(stmts: &'a [Statement<'a>]) -> Self {
        let mut d = Self::default();
        d.lexical(stmts);
        d.0.retain(|x| matches!(x.kind, DeclKind::Let | DeclKind::Const));
        d
    }

    /// eval 代码自己那一层：非严格只有顶层词法声明（`var` 进全局）；严格的连 `var` / 函数声明也在里面。
    pub fn eval_local(stmts: &'a [Statement<'a>], strict: bool) -> Self {
        if strict { Self::static_block(stmts) } else { Self::eval_lexical(stmts) }
    }

    /// 静态块：自己一层 `var` 作用域，加顶层词法声明。
    pub fn static_block(stmts: &'a [Statement<'a>]) -> Self {
        let mut d = Self::default();
        d.vars(stmts);
        d.lexical(stmts);
        d
    }

    /// 函数体那一层：`this`、自己的名字、形参、`var`、顶层词法声明。
    pub fn function(this: Option<Key>, self_name: Option<&'a BindingIdentifier<'a>>, params: Option<&'a FormalParameters<'a>>, stmts: &'a [Statement<'a>], sloppy: bool) -> Self {
        let mut d = Self::default();
        if let Some(key) = this {
            d.add(Decl { name: THIS, key, kind: DeclKind::This, ready: None });
            d.add(Decl { name: NEW_TARGET, key: NEW_TARGET_KEY | (key & POSITION_MASK), kind: DeclKind::NewTarget, ready: None });
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
        if sloppy {
            d.annexb_vars(stmts, params);
        }
        // 放最后：同名的形参 / 变量 / 函数声明优先
        if let Some(key) = this {
            d.add(Decl { name: ARGUMENTS, key: ARGUMENTS_KEY | (key & POSITION_MASK), kind: DeclKind::Arguments, ready: None });
        }
        d
    }
}

/// 类作用域里的隐藏绑定名：私有名 `#x`、私有方法的函数 `#x@method` / `@get` / `@set`、计算字段的键 `%key@位置`。
/// 这些都不是合法标识符，不会和用户的名字撞。
pub(crate) struct Hidden;

impl Hidden {
    /// `with` 语句的对象（作用域里的隐藏绑定）。
    pub fn with(alloc: &Allocator, span: Span) -> &str {
        alloc.alloc_str(&format!("%with@{}", span.start))
    }

    /// 是不是 `with` 对象的隐藏名。
    pub fn is_with(name: &str) -> bool {
        name.starts_with("%with@")
    }

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
    /// 被引用到的绑定（`new.target` / `arguments` 用到才在入口准备）
    pub used: HashSet<Key>,
    /// 当前代码是不是严格的（决定有没有 annexB 的块级函数提升）
    strict: bool,
}

impl<'a> Resolver<'a> {
    /// 脚本：顶层声明是全局对象的属性，不进作用域；只有 `this`。
    /// 给出（被捕获的绑定, 被引用到的绑定）。
    pub fn script(stmts: &'a [Statement<'a>], this: Key, alloc: &'a Allocator, local: Option<&Decls<'a>>, strict: bool) -> (HashSet<Key>, HashSet<Key>) {
        let mut r = Self { levels: Vec::new(), funcs: 0, func: 0, alloc, captured: HashSet::new(), used: HashSet::new(), strict };
        r.levels.push(Level { func: 0, names: vec![(THIS, this)] });
        if let Some(d) = local {
            r.push(d);
        }
        r.statements(stmts);
        (r.captured, r.used)
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
                self.used.insert(key);
                return;
            }
            // 经过的 `with`：名字可能落在它的对象上，对象也要拿得到
            if let Some(&(w, key)) = l.names.first()
                && Hidden::is_with(w)
            {
                if l.func != self.func {
                    self.captured.insert(key);
                }
                self.used.insert(key);
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
        let saved = self.strict;
        self.strict = saved || body.directives.iter().any(|d| d.directive.as_str() == "use strict");
        let d = Decls::function(Some(Decls::this_key(f.span)), if self_name { f.id.as_ref() } else { None }, Some(&f.params), &body.statements, !self.strict);
        let outer = self.enter_function(&d);
        self.params(&f.params);
        self.statements(&body.statements);
        self.leave_function(outer);
        self.strict = saved;
    }

    fn arrow(&mut self, a: &'a ArrowFunctionExpression<'a>) {
        let stmts: &'a [Statement<'a>] = match &a.body {
            ArrowFunctionBody::FunctionBody(b) => &b.statements,
            _ => &[],
        };
        let saved = self.strict;
        if let ArrowFunctionBody::FunctionBody(b) = &a.body {
            self.strict = saved || b.directives.iter().any(|d| d.directive.as_str() == "use strict");
        }
        let d = Decls::function(None, None, Some(&a.params), stmts, !self.strict);
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
        self.strict = saved;
    }

    /// 类：`extends`、类作用域、方法、计算键、字段初始化与静态块（各自一个合成函数）。类体全是严格代码。
    fn class(&mut self, c: &'a Class<'a>) {
        let saved = self.strict;
        self.strict = true;
        self.class_body(c);
        self.strict = saved;
    }

    fn class_body(&mut self, c: &'a Class<'a>) {
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
            let outer = self.enter_function(&Decls::function(Some(key), None, None, &[], false));
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

    /// `if` 的分支：直接是函数声明的当成只有它的块（annexB B.3.3）。
    fn clause(&mut self, s: &'a Statement<'a>) {
        if matches!(s, Statement::FunctionDeclaration(_)) {
            let one = core::slice::from_ref(s);
            self.scoped(&Decls::block([one]), |r| r.statements(one));
        } else {
            self.statement(s);
        }
    }

    fn statement(&mut self, s: &'a Statement<'a>) {
        match s {
            Statement::ExpressionStatement(e) => self.expr(&e.expression),
            Statement::VariableDeclaration(d) => self.declaration(d),
            Statement::FunctionDeclaration(f) => {
                // annexB：块级函数执行到声明处要写外层的 `var`
                if !self.strict
                    && let Some(key) = Decls::annexb_key(f)
                {
                    self.used.insert(key);
                }
                self.function(f, false);
            }
            Statement::ClassDeclaration(c) => self.class(c),
            Statement::BlockStatement(b) => self.scoped(&Decls::block([b.body.as_slice()]), |r| r.statements(&b.body)),
            Statement::IfStatement(i) => {
                self.expr(&i.test);
                self.clause(&i.consequent);
                if let Some(a) = &i.alternate {
                    self.clause(a);
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
            Statement::WithStatement(w) => {
                self.expr(&w.object);
                let hidden = Hidden::with(self.alloc, w.span);
                self.scoped(&Decls::with_scope(hidden, w.span), |r| r.statement(&w.body));
            }
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
            Expression::NewTarget(_) => self.reference(NEW_TARGET),
            Expression::TaggedTemplateExpression(t) => {
                self.expr(&t.tag);
                t.quasi.expressions.iter().for_each(|e| self.expr(e));
            }
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
