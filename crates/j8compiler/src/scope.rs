//! Scopes: which names each level declares ([`Decls`]), and which bindings are captured by inner functions
//! ([`Resolver`]).
//!
//! The pre-scan and code generation use the same [`Decls`] so that name resolution agrees: captured bindings live in
//! heap environments, the rest in registers.
use std::collections::{HashMap, HashSet};

use oxc_ast::ast::{
    Argument, ArrayExpressionElement, ArrowFunctionBody, AssignmentTargetMaybeDefault, AssignmentTargetProperty, CatchClause, ChainElement, Class, ClassElement, ArrowFunctionExpression, AssignmentTarget, BindingIdentifier, BindingPattern, Expression, ForStatementInit, ForStatementLeft, FormalParameters, Function, MemberExpression, MethodDefinitionKind, ObjectPropertyKind, PrivateIdentifier, PropertyKey, SimpleAssignmentTarget, Statement, VariableDeclaration, VariableDeclarationKind,
};
use oxc_allocator::Allocator;
use oxc_span::Span;

/// A binding's identity: the position of its declaration (`this` has a separate namespace).
pub(crate) type Key = u64;

/// Keys of `this` are kept apart from keys of ordinary bindings
const THIS_KEY: Key = 1 << 32;

/// Key space of the `new.target` pseudo-binding
const NEW_TARGET_KEY: Key = 2 << 32;

/// Key space of the `arguments` pseudo-binding
const ARGUMENTS_KEY: Key = 3 << 32;

/// The source position part of a key
const POSITION_MASK: Key = (1 << 32) - 1;

/// The `var` binding an annexB block-level function creates in the outer scope: key = this tag | position of the
/// function name
const ANNEXB_KEY: Key = 4 << 32;

/// The object where a function containing a direct eval keeps eval-declared `var`s: key = this tag | position of the
/// parameter list
const EVAL_VARS_KEY: Key = 5 << 32;

/// Outer bindings seen by direct eval code: key = this tag | index
const OUTER_KEY: Key = 6 << 32;

/// `this` can never be an identifier, so it serves as a pseudo-binding name
pub(crate) const THIS: &str = "this";

/// `new.target` is not an identifier either
pub(crate) const NEW_TARGET: &str = "new.target";

/// `arguments` in a non-arrow function, not shadowed by another declaration
pub(crate) const ARGUMENTS: &str = "arguments";

/// The kind of a declaration.
#[derive(Clone, Copy)]
pub(crate) enum DeclKind<'a> {
    /// `this` of a non-arrow function (`r0`)
    This,
    /// The own name inside a named function expression (`r1`)
    SelfName,
    /// The n-th parameter
    Param(u16),
    Var,
    Let,
    Const,
    /// Function declaration: the closure is created on entering the scope
    Function(&'a Function<'a>),
    /// `new.target` of a non-arrow function (fetched at entry only if used)
    NewTarget,
    /// The `arguments` object of a non-arrow function (created at entry only if used)
    Arguments,
}

#[derive(Clone, Copy)]
pub(crate) struct Decl<'a> {
    pub name: &'a str,
    pub key: Key,
    pub kind: DeclKind<'a>,
    /// `let` / `const` / `class`: initialized only after the declaration at this position (temporal dead zone); none
    /// for the rest
    pub ready: Option<u32>,
}

/// The names declared by one scope level, one per name (a later function declaration overrides earlier ones).
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

    /// A binding with a temporal dead zone (`ready` is where the declaration ends).
    fn lexical_ident(id: &'a BindingIdentifier<'a>, kind: DeclKind<'a>, ready: u32) -> Decl<'a> {
        Decl { ready: Some(ready), ..Self::ident(id, kind) }
    }

    /// All binding names in a pattern (`ready`: where the temporal dead zone ends).
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

    /// The bindings in a variable declaration (including destructuring).
    pub(crate) fn declarators(&mut self, d: &'a VariableDeclaration<'a>, kind: DeclKind<'a>) {
        let lexical = matches!(kind, DeclKind::Let | DeclKind::Const);
        for v in &d.declarations {
            self.pattern(&v.id, kind, lexical.then_some(v.span.end));
        }
    }

    /// `var`: passes through blocks, does not enter functions.
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

    /// Lexical declarations (`let` / `const` / function declarations), at this level only.
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

    /// annexB B.3.2: block-level ordinary function declarations that may also create a `var` in the outer scope
    /// (replacing them with `var F` would not conflict with lexical declarations along the way or with parameters).
    /// `outer` holds the parameters and lexical names at the function's top level.
    pub fn annexb(stmts: &'a [Statement<'a>], outer: Vec<&'a str>) -> Vec<&'a Function<'a>> {
        let mut out = Vec::new();
        let mut stack = alloc::vec![outer];
        for s in stmts {
            Self::annexb_stmt(s, &mut stack, &mut out);
        }
        out
    }

    /// Sloppy function: the extra `var`s annexB block-level functions add at the function level (not added if one with
    /// the same name exists).
    fn annexb_vars(&mut self, stmts: &'a [Statement<'a>], params: Option<&'a FormalParameters<'a>>) {
        for f in Self::annexb_functions(stmts, params) {
            if let (Some(id), Some(key)) = (&f.id, Self::annexb_key(f)) {
                self.add(Decl { name: id.name.as_str(), key, kind: DeclKind::Var, ready: None });
            }
        }
    }

    /// Block-level functions in a function body (or a script, with empty `params`) that can be hoisted per annexB
    /// (except `arguments`).
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

    /// Script: names of annexB block-level functions become global `var` declarations.
    pub fn annexb_globals(&mut self, hoisted: &[&'a Function<'a>]) {
        for f in hoisted {
            if let (Some(id), Some(key)) = (&f.id, Self::annexb_key(f)) {
                self.add(Decl { name: id.name.as_str(), key, kind: DeclKind::Var, ready: None });
            }
        }
    }

    /// The key of an annexB `var` binding.
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
                    // A simple catch parameter can be redeclared by var (B.3.4), a destructured one cannot
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

    /// The level just outside a `with` body: only the hidden binding of the with object.
    pub fn with_scope(name: &'a str, span: Span) -> Self {
        Self(alloc::vec![Decl { name, key: Self::key(span), kind: DeclKind::Let, ready: None }])
    }

    /// A block (or all the cases of a `switch`).
    pub fn block(stmts: impl IntoIterator<Item = &'a [Statement<'a>]>) -> Self {
        let mut d = Self::default();
        stmts.into_iter().for_each(|s| d.lexical(s));
        d
    }

    /// The head of `for (let …; …)`.
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

    /// The head of `for (let x of …)` / `for (const k in …)` (one per iteration).
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

    /// The level of the `catch (e)` parameter.
    pub fn catch_param(c: &'a CatchClause<'a>) -> Self {
        let mut d = Self::default();
        if let Some(p) = &c.param {
            d.pattern(&p.pattern, DeclKind::Let, None);
        }
        d
    }

    /// Class scope: the class's own name (used in the class body, for both declarations and expressions), plus hidden
    /// bindings for each private name, private method function, and computed field key.
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

    /// Top-level `let` / `const` / `class` of eval code (function declarations are still global).
    pub fn eval_lexical(stmts: &'a [Statement<'a>]) -> Self {
        let mut d = Self::default();
        d.lexical(stmts);
        d.0.retain(|x| matches!(x.kind, DeclKind::Let | DeclKind::Const));
        d
    }

    /// The eval code's own level: sloppy has only top-level lexical declarations (`var` goes to the global); strict has
    /// `var` / function declarations in it too.
    pub fn eval_local(stmts: &'a [Statement<'a>], strict: bool) -> Self {
        if strict { Self::static_block(stmts) } else { Self::eval_lexical(stmts) }
    }

    /// Static block: its own `var` scope, plus top-level lexical declarations.
    pub fn static_block(stmts: &'a [Statement<'a>]) -> Self {
        let mut d = Self::default();
        d.vars(stmts);
        d.lexical(stmts);
        d
    }

    /// The function body level: `this`, the own name, parameters, `var`s, top-level lexical declarations.
    pub fn function(this: Option<Key>, self_name: Option<&'a BindingIdentifier<'a>>, params: Option<&'a FormalParameters<'a>>, stmts: &'a [Statement<'a>], sloppy: bool, eval_vars: Option<&'a str>) -> Self {
        let mut d = Self::default();
        // Placed first: when name resolution reaches the function level without a match, this is checked first (same as
        // `with`)
        if let (Some(name), Some(p)) = (eval_vars, params) {
            d.add(Decl { name, key: EVAL_VARS_KEY | Self::key(p.span), kind: DeclKind::Var, ready: None });
        }
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
                // Destructured parameter: the register holds the original value, the bindings are allocated separately
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
        // Placed last: a parameter / variable / function declaration with the same name takes precedence
        if let Some(key) = this {
            d.add(Decl { name: ARGUMENTS, key: ARGUMENTS_KEY | (key & POSITION_MASK), kind: DeclKind::Arguments, ready: None });
        }
        d
    }
}

/// Hidden binding names in a class scope: private name `#x`, private method functions `#x@method` / `@get` / `@set`,
/// computed field key `%key@<pos>`. None of these are valid identifiers, so they never clash with user names.
pub(crate) struct Hidden;

impl Hidden {
    /// The catch variable of a `catch` clause with a pattern (V8's `.catch`)
    pub const CATCH: &str = ".catch";

    /// The object where a sloppy function containing a direct eval keeps eval-declared `var`s (treated as a `with`
    /// object).
    pub fn eval_vars<'a>(alloc: &'a Allocator, params: &FormalParameters<'_>) -> &'a str {
        alloc.alloc_str(&format!("%with@eval@{}", params.span.start))
    }

    /// Whether this is the object holding eval `var`s.
    pub fn is_eval_vars(name: &str) -> bool {
        name.starts_with("%with@eval@")
    }

    /// The object of a `with` statement (a hidden binding in the scope).
    pub fn with(alloc: &Allocator, span: Span) -> &str {
        alloc.alloc_str(&format!("%with@{}", span.start))
    }

    /// Whether this is the hidden name of a `with` object.
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

/// One pre-scanned scope level: which function it belongs to and what it declares.
struct Level<'a> {
    func: u32,
    names: Vec<(&'a str, Key)>,
}

/// Pre-scan: finds the bindings referenced by inner functions.
pub(crate) struct Resolver<'a> {
    levels: Vec<Level<'a>>,
    funcs: u32,
    func: u32,
    alloc: &'a Allocator,
    pub captured: HashSet<Key>,
    /// Referenced bindings (`new.target` / `arguments` are prepared at entry only if used)
    pub used: HashSet<Key>,
    /// Whether the current code is strict (decides whether annexB block-level function hoisting applies)
    strict: bool,
    /// Scopes containing a direct eval (marked by oxc_semantic, including ancestors)
    eval_scopes: HashSet<oxc_syntax::scope::ScopeId>,
    /// Functions containing a direct eval (by start position of the parameter list), handed to the compiler
    pub eval_fns: HashSet<u32>,
    /// Suspend points of each generator / async function (V8's `suspend_count`: the initial yield, each `yield` /
    /// `await`, two per `for await`, one per `yield*` (four in an async generator), one per `return` with an operand
    /// in an async generator), by start position of the parameter list
    pub suspends: HashMap<u32, u32>,
    /// Suspend points counted so far in the current function
    suspend_count: u32,
    /// The current function is an async generator
    async_generator: bool,
}

impl<'a> Resolver<'a> {
    /// Script: top-level declarations are properties of the global object and do not enter a scope; only `this` does.
    /// Gives (captured bindings, referenced bindings).
    /// `outer` is the outer bindings visible at a direct eval (innermost first, one environment per level); when
    /// present, eval code does not create its own `this`.
    pub fn script(stmts: &'a [Statement<'a>], this: Key, alloc: &'a Allocator, local: Option<&Decls<'a>>, strict: bool, eval_scopes: HashSet<oxc_syntax::scope::ScopeId>, outer: Option<&[Vec<&'a str>]>) -> Self {
        let mut r = Self { levels: Vec::new(), funcs: 0, func: 0, alloc, captured: HashSet::new(), used: HashSet::new(), strict, eval_scopes, eval_fns: HashSet::new(), suspends: HashMap::new(), suspend_count: 0, async_generator: false };
        let mut n = 0;
        for level in outer.unwrap_or(&[]).iter().rev() {
            let names = level.iter().map(|&name| {
                n += 1;
                (name, OUTER_KEY | n)
            });
            r.levels.push(Level { func: u32::MAX, names: names.collect() });
        }
        if outer.is_none() {
            r.levels.push(Level { func: 0, names: vec![(THIS, this)] });
        }
        if let Some(d) = local {
            r.push(d);
        }
        r.statements(stmts);
        r
    }

    /// Enters a function's suspend counting (a generator starts with its initial yield); gives the outer function's
    /// state back
    fn enter_suspends(&mut self, generator: bool, asynchronous: bool) -> (u32, bool) {
        let outer = (self.suspend_count, self.async_generator);
        self.suspend_count = u32::from(generator);
        self.async_generator = generator && asynchronous;
        outer
    }

    /// Leaves a function's suspend counting, recording the count of a resumable function
    fn leave_suspends(&mut self, params: &FormalParameters<'_>, resumable: bool, outer: (u32, bool)) {
        if resumable {
            self.suspends.insert(params.span.start, self.suspend_count);
        }
        (self.suspend_count, self.async_generator) = outer;
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
            // A `with` passed through: the name may resolve to its object, so the object must be reachable too
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

    /// Every binding visible to a direct eval must be in an environment: mark everything currently on the stack as
    /// captured.
    fn capture_all(&mut self) {
        for l in &self.levels {
            for &(_, key) in &l.names {
                self.captured.insert(key);
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
        let eval = f.scope_id.get().is_some_and(|id| self.eval_scopes.contains(&id));
        if eval {
            self.eval_fns.insert(f.params.span.start);
        }
        let vars = (eval && !self.strict).then(|| Hidden::eval_vars(self.alloc, &f.params));
        let d = Decls::function(Some(Decls::this_key(f.span)), if self_name { f.id.as_ref() } else { None }, Some(&f.params), &body.statements, !self.strict, vars);
        let outer = self.enter_function(&d);
        let suspends = self.enter_suspends(f.generator, f.r#async);
        if eval {
            self.capture_all();
        }
        self.params(&f.params);
        self.statements(&body.statements);
        self.leave_suspends(&f.params, f.generator || f.r#async, suspends);
        // Sloppy, simple parameters, and arguments is used: the parameters go into the environment and arguments
        // indices map onto them
        let simple = f.params.rest.is_none() && f.params.items.iter().all(|i| i.initializer.is_none() && matches!(i.pattern, BindingPattern::BindingIdentifier(_)));
        let args_key = ARGUMENTS_KEY | (Decls::this_key(f.span) & POSITION_MASK);
        if !self.strict && simple && self.used.contains(&args_key) {
            for d in d.0.iter().filter(|d| matches!(d.kind, DeclKind::Param(_))) {
                self.captured.insert(d.key);
            }
        }
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
        let eval = a.scope_id.get().is_some_and(|id| self.eval_scopes.contains(&id));
        if eval {
            self.eval_fns.insert(a.params.span.start);
        }
        let vars = (eval && !self.strict).then(|| Hidden::eval_vars(self.alloc, &a.params));
        let d = Decls::function(None, None, Some(&a.params), stmts, !self.strict, vars);
        let outer = self.enter_function(&d);
        let suspends = self.enter_suspends(false, a.r#async);
        if eval {
            self.capture_all();
        }
        self.params(&a.params);
        match &a.body {
            ArrowFunctionBody::FunctionBody(_) => self.statements(stmts),
            e => {
                if let Some(e) = e.as_expression() {
                    self.expr(e);
                }
            }
        }
        self.leave_suspends(&a.params, a.r#async, suspends);
        self.leave_function(outer);
        self.strict = saved;
    }

    /// Class: `extends`, the class scope, methods, computed keys, field initializers and static blocks (each its own
    /// synthetic function). A class body is entirely strict code.
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
            let outer = self.enter_function(&Decls::function(Some(key), None, None, &[], false, None));
            for e in &c.body.body {
                match e {
                    // The instance initializer installs private methods: read the private names and method functions
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

    /// Default values and computed keys in a pattern.
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

    /// Names, objects, keys and default values referenced in an assignment target (including destructuring assignment).
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

    /// Branches of `if`: a bare function declaration is treated as a block containing only it (annexB B.3.3).
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
                // annexB: when execution reaches the declaration, a block-level function writes the outer `var`
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
            Statement::ForOfStatement(f) => {
                if f.r#await {
                    // One suspend for `next()`, one for `return()`
                    self.suspend_count += 2;
                }
                self.for_each(&f.left, &f.right, &f.body);
            }
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
                    // An async generator awaits the operand
                    self.suspend_count += u32::from(self.async_generator);
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

    /// `for-in` / `for-of`: the right side is evaluated outside, the left side and the body in the per-iteration level.
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

    #[expect(clippy::too_many_lines, reason = "one arm per expression kind")]
    fn expr(&mut self, e: &'a Expression<'a>) {
        match e {
            Expression::Identifier(id) => self.reference(id.name.as_str()),
            // `super.x` / `super(...)` both read and write `this`
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
            Expression::ImportExpression(i) => {
                self.expr(&i.source);
                if let Some(o) = &i.options {
                    self.expr(o);
                }
            }
            Expression::TaggedTemplateExpression(t) => {
                self.expr(&t.tag);
                t.quasi.expressions.iter().for_each(|e| self.expr(e));
            }
            Expression::YieldExpression(y) => {
                // `yield*` in an async generator: also the return, iterator close and delegated output suspends
                self.suspend_count += if y.delegate && self.async_generator { 4 } else { 1 };
                if let Some(a) = &y.argument {
                    self.expr(a);
                }
            }
            Expression::AwaitExpression(a) => {
                self.suspend_count += 1;
                self.expr(&a.argument);
            }
            Expression::UpdateExpression(u) => self.simple_target(&u.argument),
            Expression::CallExpression(c) => {
                if matches!(&c.callee, Expression::Identifier(id) if id.name == "eval") {
                    self.capture_all();
                }
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
