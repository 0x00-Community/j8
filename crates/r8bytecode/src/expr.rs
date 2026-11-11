//! JS ⇒ 字节码：表达式。结果都留在 acc。
use oxc_ast::ast::{
    Argument, ArrayAssignmentTarget, ArrayExpressionElement, ArrowFunctionBody, AssignmentTarget, AssignmentTargetMaybeDefault, AssignmentTargetProperty, ChainElement, ComputedMemberExpression, PrivateFieldExpression, StaticMemberExpression, TaggedTemplateExpression, ArrowFunctionExpression, AssignmentExpression, AssignmentOperator, BinaryExpression, BinaryOperator, CallExpression, Expression, LogicalOperator, MemberExpression, MethodDefinitionKind, ObjectExpression, ObjectPropertyKind, PropertyKind as PropertyKind_, SimpleAssignmentTarget, TemplateElement, UnaryOperator, UpdateExpression,
    UpdateOperator,
};
use oxc_span::{GetSpan, Span};
use r8frame::{BinaryOp, CompareOp, Error, Resume, UnaryOp};

use crate::compiler::{Body, Compiler, Flavor, Place};
use crate::scope::Hidden;
use crate::{Const, Constant, FunctionKind, Insn, PropertyKind, Reg};

/// 二元运算符 ⇒ 字节码运算。
enum Operator {
    Binary(BinaryOp),
    Compare(CompareOp),
    InstanceOf,
    In,
}

/// 赋值运算符的三类。
enum Assign {
    Plain,
    Binary(BinaryOp),
    /// `||=` / `&&=` / `??=`：左边已经满足就不求右边
    Logical(fn(crate::Label) -> Insn),
}

/// 数组字面量 / 实参里的一个元素。
enum Element<'a> {
    Value(&'a Expression<'a>),
    Spread(&'a Expression<'a>),
    Hole,
}

impl<'a> From<&'a ArrayExpressionElement<'a>> for Element<'a> {
    fn from(e: &'a ArrayExpressionElement<'a>) -> Self {
        match e {
            ArrayExpressionElement::SpreadElement(s) => Element::Spread(&s.argument),
            ArrayExpressionElement::Elision(_) => Element::Hole,
            e => Element::Value(e.as_expression().expect("the rest are expressions")),
        }
    }
}

impl<'a> From<&'a Argument<'a>> for Element<'a> {
    fn from(a: &'a Argument<'a>) -> Self {
        match a {
            Argument::SpreadElement(s) => Element::Spread(&s.argument),
            a => Element::Value(a.as_expression().expect("the rest are expressions")),
        }
    }
}

/// 赋值目标：名字，或已经求好对象（与键）的属性。
enum Target<'a> {
    Name(&'a str),
    Named { obj: Reg, name: Const },
    Keyed { obj: Reg, key: Reg },
    /// `obj.#x`：私有名在 `key`
    Private { obj: Reg, key: Reg },
}

impl<'a> Compiler<'a, '_> {
    fn operator(op: BinaryOperator) -> Operator {
        match op {
            BinaryOperator::Addition => Operator::Binary(BinaryOp::Add),
            BinaryOperator::Subtraction => Operator::Binary(BinaryOp::Sub),
            BinaryOperator::Multiplication => Operator::Binary(BinaryOp::Mul),
            BinaryOperator::Division => Operator::Binary(BinaryOp::Div),
            BinaryOperator::Remainder => Operator::Binary(BinaryOp::Rem),
            BinaryOperator::Exponential => Operator::Binary(BinaryOp::Exp),
            BinaryOperator::BitwiseAnd => Operator::Binary(BinaryOp::BitAnd),
            BinaryOperator::BitwiseOR => Operator::Binary(BinaryOp::BitOr),
            BinaryOperator::BitwiseXOR => Operator::Binary(BinaryOp::BitXor),
            BinaryOperator::ShiftLeft => Operator::Binary(BinaryOp::Shl),
            BinaryOperator::ShiftRight => Operator::Binary(BinaryOp::Shr),
            BinaryOperator::ShiftRightZeroFill => Operator::Binary(BinaryOp::UShr),
            BinaryOperator::Equality => Operator::Compare(CompareOp::Eq),
            BinaryOperator::Inequality => Operator::Compare(CompareOp::Ne),
            BinaryOperator::StrictEquality => Operator::Compare(CompareOp::StrictEq),
            BinaryOperator::StrictInequality => Operator::Compare(CompareOp::StrictNe),
            BinaryOperator::LessThan => Operator::Compare(CompareOp::Lt),
            BinaryOperator::GreaterThan => Operator::Compare(CompareOp::Gt),
            BinaryOperator::LessEqualThan => Operator::Compare(CompareOp::Le),
            BinaryOperator::GreaterEqualThan => Operator::Compare(CompareOp::Ge),
            BinaryOperator::In => Operator::In,
            BinaryOperator::Instanceof => Operator::InstanceOf,
        }
    }

    fn assign_kind(op: AssignmentOperator) -> Assign {
        match op {
            AssignmentOperator::Assign => Assign::Plain,
            AssignmentOperator::Addition => Assign::Binary(BinaryOp::Add),
            AssignmentOperator::Subtraction => Assign::Binary(BinaryOp::Sub),
            AssignmentOperator::Multiplication => Assign::Binary(BinaryOp::Mul),
            AssignmentOperator::Division => Assign::Binary(BinaryOp::Div),
            AssignmentOperator::Remainder => Assign::Binary(BinaryOp::Rem),
            AssignmentOperator::Exponential => Assign::Binary(BinaryOp::Exp),
            AssignmentOperator::ShiftLeft => Assign::Binary(BinaryOp::Shl),
            AssignmentOperator::ShiftRight => Assign::Binary(BinaryOp::Shr),
            AssignmentOperator::ShiftRightZeroFill => Assign::Binary(BinaryOp::UShr),
            AssignmentOperator::BitwiseOR => Assign::Binary(BinaryOp::BitOr),
            AssignmentOperator::BitwiseXOR => Assign::Binary(BinaryOp::BitXor),
            AssignmentOperator::BitwiseAnd => Assign::Binary(BinaryOp::BitAnd),
            AssignmentOperator::LogicalOr => Assign::Logical(Insn::JumpIfTrue),
            AssignmentOperator::LogicalAnd => Assign::Logical(Insn::JumpIfFalse),
            AssignmentOperator::LogicalNullish => Assign::Logical(Insn::JumpIfNotNullish),
        }
    }

    pub(crate) fn string_const(&mut self, s: Option<&str>, span: Span) -> Result<Const, Error> {
        let s = s.ok_or_else(|| self.unsupported(span, "lone surrogates"))?;
        Ok(self.constant(Constant::String(s.into())))
    }

    /// 模板字面量的一段（按转义处理后的文本）。
    fn quasi(&mut self, q: &TemplateElement<'a>) -> Result<Const, Error> {
        let cooked = match q.value.cooked {
            Some(c) => c.as_str(),
            None => None,
        };
        self.string_const(cooked, q.span)
    }

    #[expect(clippy::too_many_lines, reason = "一种表达式一个分支，拆开反而难读")]
    pub(crate) fn expr(&mut self, e: &'a Expression<'a>) -> Result<(), Error> {
        match e {
            Expression::NumericLiteral(n) => {
                let k = self.constant(Constant::Number(n.value));
                self.emit(Insn::LdaConst(k));
            }
            Expression::StringLiteral(s) => {
                let k = self.string_const(s.value.as_str(), s.span)?;
                self.emit(Insn::LdaConst(k));
            }
            Expression::BooleanLiteral(b) => _ = self.emit(Insn::LdaBool(b.value)),
            Expression::BigIntLiteral(b) => {
                let k = self.constant(Constant::BigInt((&*b.value).into()));
                self.emit(Insn::LdaConst(k));
            }
            Expression::RegExpLiteral(r) => {
                let pattern = self.name(&r.regex.pattern.text);
                let flags = self.name(&r.regex.flags.to_string());
                self.emit(Insn::CreateRegExp { pattern, flags });
            }
            Expression::NullLiteral(_) => _ = self.emit(Insn::LdaNull),
            Expression::TemplateLiteral(t) => {
                // q0 + e0 + q1 + … ：左边是字符串，`+` 就是 ToString 后拼接
                let first = t.quasis.first().expect("a template has a first quasi");
                let k = self.quasi(first)?;
                self.emit(Insn::LdaConst(k));
                let acc = self.temp();
                for (e, q) in t.expressions.iter().zip(t.quasis.iter().skip(1)) {
                    self.emit(Insn::Star(acc));
                    self.expr(e)?;
                    self.emit(Insn::Binary(BinaryOp::Add, acc));
                    self.emit(Insn::Star(acc));
                    let k = self.quasi(q)?;
                    self.emit(Insn::LdaConst(k));
                    self.emit(Insn::Binary(BinaryOp::Add, acc));
                }
                self.release(acc);
            }
            Expression::Identifier(id) => self.load_at(id.name.as_str(), id.span.start),
            Expression::ThisExpression(t) => self.load_at(crate::scope::THIS, t.span.start),
            Expression::ParenthesizedExpression(p) => self.expr(&p.expression)?,
            Expression::FunctionExpression(f) => {
                let n = self.plain_function(f, true, "")?;
                self.emit(Insn::CreateClosure(n));
            }
            Expression::ArrowFunctionExpression(a) => self.arrow(a, "")?,
            Expression::ClassExpression(c) => self.class(c, "")?,
            Expression::NewTarget(t) => self.load_at(crate::scope::NEW_TARGET, t.span.start),
            Expression::TaggedTemplateExpression(t) => self.tagged_template(t)?,
            Expression::YieldExpression(y) => {
                let mode = self.resume_reg(y.span, "yield outside a generator")?;
                let async_generator = self.is_async_generator();
                match (&y.argument, y.delegate) {
                    (Some(a), true) if async_generator => self.yield_star_async(a, mode)?,
                    (Some(a), true) => self.yield_star(a, mode)?,
                    (a, _) => {
                        match a {
                            Some(a) => self.expr(a)?,
                            None => _ = self.emit(Insn::LdaUndefined),
                        }
                        if async_generator {
                            // async 生成器里 `yield v` 先等 v
                            self.emit(Insn::Await { mode });
                            self.resume_dispatch(mode, false)?;
                        }
                        self.emit(Insn::Yield { mode });
                        self.resume_dispatch(mode, true)?;
                    }
                }
            }
            Expression::AwaitExpression(e) => {
                let mode = self.resume_reg(e.span, "await outside an async function")?;
                self.expr(&e.argument)?;
                self.emit(Insn::Await { mode });
                self.resume_dispatch(mode, false)?;
            }
            Expression::NewExpression(n) => {
                if Self::has_spread(&n.arguments) {
                    let callee = self.temp();
                    self.expr(&n.callee)?;
                    self.emit(Insn::Star(callee));
                    let args = self.spread_array(n.arguments.iter().map(Element::from))?;
                    self.emit(Insn::ConstructSpread { callee, args });
                    self.release(callee);
                } else {
                    let argc = u16::try_from(n.arguments.len()).map_err(|_| self.unsupported(n.span, "more than 65535 arguments"))?;
                    let callee = self.temps(1 + argc);
                    self.expr(&n.callee)?;
                    self.emit(Insn::Star(callee));
                    let args = Reg(callee.0 + 1);
                    self.arguments(&n.arguments, args)?;
                    self.emit(Insn::Construct { callee, args, argc });
                    self.release(callee);
                }
            }
            Expression::UnaryExpression(u) => {
                let op = match u.operator {
                    UnaryOperator::UnaryNegation => UnaryOp::Neg,
                    UnaryOperator::UnaryPlus => UnaryOp::Plus,
                    UnaryOperator::LogicalNot => UnaryOp::Not,
                    UnaryOperator::BitwiseNot => UnaryOp::BitNot,
                    UnaryOperator::Typeof => UnaryOp::Typeof,
                    UnaryOperator::Void => UnaryOp::Void,
                    UnaryOperator::Delete => return self.delete(&u.argument),
                };
                match (&u.argument, op) {
                    // `typeof 未声明的名字` 不抛错
                    (Expression::Identifier(id), UnaryOp::Typeof) if matches!(self.lookup(id.name.as_str()).0, Place::Global(g) if !matches!(g, "undefined" | "NaN" | "Infinity")) => {
                        let k = self.name(id.name.as_str());
                        self.emit(Insn::LdaGlobalOrUndefined(k));
                    }
                    (a, _) => self.expr(a)?,
                }
                self.emit(Insn::Unary(op));
            }
            Expression::BinaryExpression(b) => self.binary(b)?,
            Expression::LogicalExpression(l) => {
                self.expr(&l.left)?;
                let skip = self.jump(match l.operator {
                    LogicalOperator::Or => Insn::JumpIfTrue,
                    LogicalOperator::And => Insn::JumpIfFalse,
                    LogicalOperator::Coalesce => Insn::JumpIfNotNullish,
                });
                self.expr(&l.right)?;
                self.land(skip);
            }
            Expression::ConditionalExpression(c) => {
                self.expr(&c.test)?;
                let other = self.jump(Insn::JumpIfFalse);
                self.expr(&c.consequent)?;
                let end = self.jump(Insn::Jump);
                self.land(other);
                self.expr(&c.alternate)?;
                self.land(end);
            }
            Expression::SequenceExpression(s) => s.expressions.iter().try_for_each(|e| self.expr(e))?,
            Expression::AssignmentExpression(a) => self.assignment(a)?,
            Expression::UpdateExpression(u) => self.update(u)?,
            Expression::CallExpression(c) => self.call(c)?,
            Expression::StaticMemberExpression(m) if matches!(m.object, Expression::Super(_)) => {
                let this = self.this_temp();
                let name = self.name(m.property.name.as_str());
                self.emit(Insn::GetSuper { this, name });
                self.release(this);
            }
            Expression::ComputedMemberExpression(m) if matches!(m.object, Expression::Super(_)) => {
                let this = self.this_temp();
                self.expr(&m.expression)?;
                self.emit(Insn::GetSuperKeyed { this });
                self.release(this);
            }
            Expression::StaticMemberExpression(m) => self.static_member(m)?,
            Expression::PrivateFieldExpression(m) => self.private_member(m)?,
            Expression::PrivateInExpression(p) => {
                self.expr(&p.right)?;
                let obj = self.temp();
                self.emit(Insn::Star(obj));
                self.load(Hidden::private(self.alloc, &p.left));
                self.emit(Insn::HasPrivate { obj });
                self.release(obj);
            }
            Expression::ComputedMemberExpression(m) => self.computed_member(m)?,
            Expression::ChainExpression(c) => {
                self.chains.push(Vec::new());
                match &c.expression {
                    ChainElement::CallExpression(call) => self.call(call)?,
                    ChainElement::StaticMemberExpression(m) => self.static_member(m)?,
                    ChainElement::ComputedMemberExpression(m) => self.computed_member(m)?,
                    ChainElement::PrivateFieldExpression(m) => self.private_member(m)?,
                    ChainElement::TSNonNullExpression(e) => return Err(self.unsupported(e.span, "TypeScript non-null assertions")),
                }
                let jumps = self.chains.pop().expect("this chain");
                if !jumps.is_empty() {
                    let end = self.jump(Insn::Jump);
                    for j in jumps {
                        self.land(j);
                    }
                    self.emit(Insn::LdaUndefined);
                    self.land(end);
                }
            }
            Expression::ObjectExpression(o) => self.object(o)?,
            Expression::ArrayExpression(a) => {
                if a.elements.iter().all(|e| e.as_expression().is_some()) {
                    let count = u16::try_from(a.elements.len()).map_err(|_| self.unsupported(a.span, "more than 65535 elements"))?;
                    let first = self.temps(count);
                    for (i, el) in (0..).zip(&a.elements) {
                        self.expr(el.as_expression().expect("checked above"))?;
                        self.emit(Insn::Star(Reg(first.0 + i)));
                    }
                    self.emit(Insn::CreateArray { first, count });
                    self.release(first);
                } else {
                    let arr = self.spread_array(a.elements.iter().map(Element::from))?;
                    self.emit(Insn::Ldar(arr));
                    self.release(arr);
                }
            }
            e => return Err(self.unsupported(e.span(), "expression")),
        }
        Ok(())
    }

    /// 匿名函数 / 箭头函数 / 匿名类按规范推断名字（NamedEvaluation）；别的照常求值。
    pub(crate) fn expr_named(&mut self, e: &'a Expression<'a>, name: &'a str) -> Result<(), Error> {
        match e {
            Expression::FunctionExpression(f) if f.id.is_none() => {
                let n = self.plain_function(f, true, name)?;
                self.emit(Insn::CreateClosure(n));
                Ok(())
            }
            Expression::ArrowFunctionExpression(a) => self.arrow(a, name),
            Expression::ClassExpression(c) if c.id.is_none() => self.class(c, name),
            Expression::ParenthesizedExpression(p) => self.expr_named(&p.expression, name),
            e => self.expr(e),
        }
    }

    /// 标签模板：`tag(模板对象, ...替换值)`，模板对象按调用点缓存（cooked 串非法转义时是 undefined）。
    fn tagged_template(&mut self, t: &'a TaggedTemplateExpression<'a>) -> Result<(), Error> {
        let subs = &t.quasi.expressions;
        let argc = u16::try_from(subs.len() + 1).map_err(|_| self.unsupported(t.span, "more than 65535 substitutions"))?;
        let callee = self.temps(2 + argc);
        let this = Reg(callee.0 + 1);
        let args = Reg(callee.0 + 2);
        match &t.tag {
            Expression::StaticMemberExpression(m) => {
                self.expr(&m.object)?;
                self.emit(Insn::Star(this));
                let name = self.name(m.property.name.as_str());
                self.emit(Insn::GetNamed { obj: this, name });
            }
            Expression::ComputedMemberExpression(m) => {
                self.expr(&m.object)?;
                self.emit(Insn::Star(this));
                self.expr(&m.expression)?;
                self.emit(Insn::GetKeyed { obj: this });
            }
            tag => {
                self.emit(Insn::LdaUndefined);
                self.emit(Insn::Star(this));
                self.expr(tag)?;
            }
        }
        self.emit(Insn::Star(callee));
        let count = u16::try_from(t.quasi.quasis.len()).map_err(|_| self.unsupported(t.span, "too many template parts"))?;
        let strings = self.temps(count * 2);
        for (i, q) in (0u16..).zip(&t.quasi.quasis) {
            match q.value.cooked.and_then(|c| c.as_str().map(str::to_owned)) {
                Some(c) => {
                    let k = self.constant(Constant::String(c.into()));
                    self.emit(Insn::LdaConst(k));
                }
                None => _ = self.emit(Insn::LdaUndefined),
            }
            self.emit(Insn::Star(Reg(strings.0 + i)));
            let raw = self.constant(Constant::String((&*q.value.raw).into()));
            self.emit(Insn::LdaConst(raw));
            self.emit(Insn::Star(Reg(strings.0 + count + i)));
        }
        let site = self.sites;
        self.sites += 1;
        self.emit(Insn::GetTemplateObject { site, first: strings, count });
        self.release(strings);
        self.emit(Insn::Star(args));
        for (i, e) in (1u16..).zip(subs) {
            self.expr(e)?;
            self.emit(Insn::Star(Reg(args.0 + i)));
        }
        self.emit(Insn::Call { callee, this, args, argc });
        self.release(callee);
        Ok(())
    }

    fn arrow(&mut self, a: &'a ArrowFunctionExpression<'a>, name: &str) -> Result<(), Error> {
        let body = match &a.body {
            ArrowFunctionBody::FunctionBody(b) => Body::Statements(&b.statements),
            e => Body::Expression(e.as_expression().expect("an arrow's body is a block or an expression")),
        };
        let n = self.function(name, Flavor { kind: FunctionKind::Arrow, generator: false, asynchronous: a.r#async }, None, None, Some(&a.params), body)?;
        self.emit(Insn::CreateClosure(n));
        Ok(())
    }

    fn binary(&mut self, b: &'a BinaryExpression<'a>) -> Result<(), Error> {
        let op = Self::operator(b.operator);
        self.expr(&b.left)?;
        let l = self.temp();
        self.emit(Insn::Star(l));
        self.expr(&b.right)?;
        self.emit(match op {
            Operator::Binary(op) => Insn::Binary(op, l),
            Operator::Compare(op) => Insn::Compare(op, l),
            Operator::InstanceOf => Insn::InstanceOf(l),
            Operator::In => Insn::In(l),
        });
        self.release(l);
        Ok(())
    }

    /// 求出赋值目标的对象（与键）放进临时寄存器。
    fn target(&mut self, t: &'a SimpleAssignmentTarget<'a>) -> Result<Target<'a>, Error> {
        if let SimpleAssignmentTarget::AssignmentTargetIdentifier(id) = t {
            return Ok(Target::Name(id.name.as_str()));
        }
        match t.as_member_expression() {
            Some(MemberExpression::StaticMemberExpression(m)) => {
                self.expr(&m.object)?;
                let obj = self.temp();
                self.emit(Insn::Star(obj));
                Ok(Target::Named { obj, name: self.name(m.property.name.as_str()) })
            }
            Some(MemberExpression::ComputedMemberExpression(m)) => {
                self.expr(&m.object)?;
                let obj = self.temp();
                self.emit(Insn::Star(obj));
                self.expr(&m.expression)?;
                let key = self.temp();
                self.emit(Insn::Star(key));
                Ok(Target::Keyed { obj, key })
            }
            Some(MemberExpression::PrivateFieldExpression(m)) => {
                self.expr(&m.object)?;
                let obj = self.temp();
                self.emit(Insn::Star(obj));
                self.load(Hidden::private(self.alloc, &m.field));
                let key = self.temp();
                self.emit(Insn::Star(key));
                Ok(Target::Private { obj, key })
            }
            _ => Err(self.unsupported(t.span(), "assignment target")),
        }
    }

    fn target_load(&mut self, t: &Target<'a>) {
        match *t {
            Target::Name(n) => self.load(n),
            Target::Named { obj, name } => _ = self.emit(Insn::GetNamed { obj, name }),
            Target::Keyed { obj, key } => {
                self.emit(Insn::Ldar(key));
                self.emit(Insn::GetKeyed { obj });
            }
            Target::Private { obj, key } => {
                self.emit(Insn::Ldar(key));
                self.emit(Insn::GetPrivate { obj });
            }
        }
    }

    fn target_store(&mut self, t: &Target<'a>) {
        match *t {
            Target::Name(n) => self.store(n, false),
            Target::Named { obj, name } => _ = self.emit(Insn::SetNamed { obj, name }),
            Target::Keyed { obj, key } => _ = self.emit(Insn::SetKeyed { obj, key }),
            Target::Private { obj, key } => _ = self.emit(Insn::SetPrivate { obj, key }),
        }
    }

    fn target_release(&mut self, t: &Target<'a>) {
        match *t {
            Target::Name(_) => {}
            Target::Named { obj, .. } | Target::Keyed { obj, .. } | Target::Private { obj, .. } => self.release(obj),
        }
    }

    fn assignment(&mut self, a: &'a AssignmentExpression<'a>) -> Result<(), Error> {
        let Some(t) = a.left.as_simple_assignment_target() else {
            self.expr(&a.right)?;
            let v = self.temp();
            self.emit(Insn::Star(v));
            self.assign_target(&a.left, v)?;
            self.emit(Insn::Ldar(v));
            self.release(v);
            return Ok(());
        };
        let t = self.target(t)?;
        match Self::assign_kind(a.operator) {
            Assign::Plain => {
                match &t {
                    Target::Name(n) => self.expr_named(&a.right, n)?,
                    _ => self.expr(&a.right)?,
                }
                self.target_store(&t);
            }
            Assign::Binary(op) => {
                self.target_load(&t);
                let old = self.temp();
                self.emit(Insn::Star(old));
                self.expr(&a.right)?;
                self.emit(Insn::Binary(op, old));
                self.release(old);
                self.target_store(&t);
            }
            Assign::Logical(skip) => {
                self.target_load(&t);
                let at = self.jump(skip);
                match &t {
                    Target::Name(n) => self.expr_named(&a.right, n)?,
                    _ => self.expr(&a.right)?,
                }
                self.target_store(&t);
                self.land(at);
            }
        }
        self.target_release(&t);
        Ok(())
    }

    fn update(&mut self, u: &'a UpdateExpression<'a>) -> Result<(), Error> {
        let t = self.target(&u.argument)?;
        self.target_load(&t);
        self.emit(Insn::ToNumeric);
        let old = (!u.prefix).then(|| self.temp());
        if let Some(r) = old {
            self.emit(Insn::Star(r));
        }
        self.emit(match u.operator {
            UpdateOperator::Increment => Insn::Inc,
            UpdateOperator::Decrement => Insn::Dec,
        });
        self.target_store(&t);
        if let Some(r) = old {
            self.emit(Insn::Ldar(r));
            self.release(r);
        }
        self.target_release(&t);
        Ok(())
    }

    /// 调用：`callee`、`this`、实参排在连续的寄存器里。
    fn call(&mut self, c: &'a CallExpression<'a>) -> Result<(), Error> {
        let spread = Self::has_spread(&c.arguments);
        let argc = if spread { 0 } else { u16::try_from(c.arguments.len()).map_err(|_| self.unsupported(c.span, "more than 65535 arguments"))? };
        if let (Expression::Super(_), true) = (&c.callee, spread) {
            let args = self.spread_array(c.arguments.iter().map(Element::from))?;
            self.emit(Insn::SuperCallSpread { args });
            self.release(args);
            self.store(crate::scope::THIS, true);
            return Ok(());
        }
        if let Expression::Super(_) = &c.callee {
            let args = self.temps(argc);
            self.arguments(&c.arguments, args)?;
            self.emit(Insn::SuperCall { args, argc });
            self.release(args);
            self.store(crate::scope::THIS, true);
            return Ok(());
        }
        let callee = self.temps(2 + argc);
        let this = Reg(callee.0 + 1);
        match &c.callee {
            Expression::StaticMemberExpression(m) if matches!(m.object, Expression::Super(_)) => {
                self.load(crate::scope::THIS);
                self.emit(Insn::Star(this));
                let name = self.name(m.property.name.as_str());
                self.emit(Insn::GetSuper { this, name });
            }
            Expression::StaticMemberExpression(m) => {
                self.expr(&m.object)?;
                self.emit(Insn::Star(this));
                self.optional(m.optional, m.span)?;
                let name = self.name(m.property.name.as_str());
                self.emit(Insn::GetNamed { obj: this, name });
            }
            Expression::ComputedMemberExpression(m) => {
                self.expr(&m.object)?;
                self.emit(Insn::Star(this));
                self.optional(m.optional, m.span)?;
                self.expr(&m.expression)?;
                self.emit(Insn::GetKeyed { obj: this });
            }
            Expression::PrivateFieldExpression(m) => {
                self.expr(&m.object)?;
                self.emit(Insn::Star(this));
                self.optional(m.optional, m.span)?;
                self.load(Hidden::private(self.alloc, &m.field));
                self.emit(Insn::GetPrivate { obj: this });
            }
            f => {
                self.emit(Insn::LdaUndefined);
                self.emit(Insn::Star(this));
                self.expr(f)?;
            }
        }
        self.optional(c.optional, c.span)?;
        self.emit(Insn::Star(callee));
        if spread {
            let args = self.spread_array(c.arguments.iter().map(Element::from))?;
            self.emit(Insn::CallSpread { callee, this, args });
            self.release(args);
        } else {
            let first = Reg(callee.0 + 2);
            self.arguments(&c.arguments, first)?;
            self.emit(Insn::Call { callee, this, args: first, argc });
        }
        self.release(callee);
        Ok(())
    }

    fn static_member(&mut self, m: &'a StaticMemberExpression<'a>) -> Result<(), Error> {
        self.expr(&m.object)?;
        self.optional(m.optional, m.span)?;
        let obj = self.temp();
        self.emit(Insn::Star(obj));
        let name = self.name(m.property.name.as_str());
        self.emit(Insn::GetNamed { obj, name });
        self.release(obj);
        Ok(())
    }

    /// `obj.#x`。
    fn private_member(&mut self, m: &'a PrivateFieldExpression<'a>) -> Result<(), Error> {
        self.expr(&m.object)?;
        self.optional(m.optional, m.span)?;
        let obj = self.temp();
        self.emit(Insn::Star(obj));
        self.load(Hidden::private(self.alloc, &m.field));
        self.emit(Insn::GetPrivate { obj });
        self.release(obj);
        Ok(())
    }

    fn computed_member(&mut self, m: &'a ComputedMemberExpression<'a>) -> Result<(), Error> {
        self.expr(&m.object)?;
        self.optional(m.optional, m.span)?;
        let obj = self.temp();
        self.emit(Insn::Star(obj));
        self.expr(&m.expression)?;
        self.emit(Insn::GetKeyed { obj });
        self.release(obj);
        Ok(())
    }

    /// `?.`：acc 是 null / undefined 就跳到当前链的末尾。
    fn optional(&mut self, optional: bool, span: Span) -> Result<(), Error> {
        if optional {
            let j = self.jump(Insn::JumpIfNullish);
            if self.chains.is_empty() {
                return Err(self.unsupported(span, "`?.` outside an optional chain"));
            }
            self.chains.last_mut().expect("checked above").push(j);
        }
        Ok(())
    }

    fn has_spread(args: &[Argument<'a>]) -> bool {
        args.iter().any(|a| matches!(a, Argument::SpreadElement(_)))
    }

    /// 带展开 / 空位的元素序列建成一个数组，放进新的临时寄存器（调用方释放）。
    fn spread_array(&mut self, elements: impl Iterator<Item = Element<'a>>) -> Result<Reg, Error> {
        let arr = self.temp();
        self.emit(Insn::CreateArray { first: arr, count: 0 });
        self.emit(Insn::Star(arr));
        for e in elements {
            match e {
                Element::Value(v) => {
                    self.expr(v)?;
                    self.emit(Insn::ArrayPush { arr });
                }
                Element::Spread(v) => {
                    self.expr(v)?;
                    self.emit(Insn::ArraySpread { arr });
                }
                Element::Hole => {
                    self.emit(Insn::LdaUndefined);
                    self.emit(Insn::ArrayPush { arr });
                }
            }
        }
        Ok(arr)
    }

    /// 解构赋值：把寄存器 `value` 里的值按赋值目标模式存进去。
    pub(crate) fn assign_target(&mut self, t: &'a AssignmentTarget<'a>, value: Reg) -> Result<(), Error> {
        match t {
            AssignmentTarget::ArrayAssignmentTarget(a) => self.assign_array(a, value)?,
            AssignmentTarget::ObjectAssignmentTarget(o) => {
                let n = u16::try_from(o.properties.len()).map_err(|_| self.unsupported(o.span, "more than 65535 properties"))?;
                let keys = self.temps(n);
                for (i, p) in (0u16..).zip(&o.properties) {
                    let k = Reg(keys.0 + i);
                    match p {
                        AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(id) => {
                            let name = id.binding.name.as_str();
                            let c = self.name(name);
                            self.emit(Insn::LdaConst(c));
                            self.emit(Insn::Star(k));
                            self.emit(Insn::GetKeyed { obj: value });
                            let v = self.temp();
                            self.emit(Insn::Star(v));
                            let v2 = match &id.init {
                                Some(d) => self.with_default(v, d, Some(name))?,
                                None => v,
                            };
                            self.emit(Insn::Ldar(v2));
                            self.store(name, false);
                            self.release(v);
                        }
                        AssignmentTargetProperty::AssignmentTargetPropertyProperty(pp) => {
                            self.property_key_into(&pp.name, pp.computed, k)?;
                            self.emit(Insn::Ldar(k));
                            self.emit(Insn::GetKeyed { obj: value });
                            let v = self.temp();
                            self.emit(Insn::Star(v));
                            self.assign_maybe_default(&pp.binding, v)?;
                            self.release(v);
                        }
                    }
                }
                if let Some(r) = &o.rest {
                    self.emit(Insn::ObjectRest { src: value, keys, count: n });
                    let v = self.temp();
                    self.emit(Insn::Star(v));
                    self.assign_target(&r.target, v)?;
                    self.release(v);
                } else if o.properties.is_empty() {
                    self.require_object(value);
                }
                self.release(keys);
            }
            t => {
                let s = t.as_simple_assignment_target().ok_or_else(|| self.unsupported(t.span(), "assignment target"))?;
                let target = self.target(s)?;
                self.emit(Insn::Ldar(value));
                self.target_store(&target);
                self.target_release(&target);
            }
        }
        Ok(())
    }

    /// 数组解构赋值：规范顺序——每个元素先求值目标引用、再走迭代器；中途抛出先关迭代器。
    fn assign_array(&mut self, a: &'a ArrayAssignmentTarget<'a>, value: Reg) -> Result<(), Error> {
        let iter = self.iterate(value);
        // 规范顺序：每个元素先求值目标引用、再走迭代器；中途抛出先关迭代器
        self.closing_on_throw(iter, |c| {
            for e in &a.elements {
                let Some(e) = e else {
                    c.emit(Insn::IteratorNext { iter });
                    continue;
                };
                let (binding, default) = match e {
                    AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => (&d.binding, Some(&d.init)),
                    t => (t.as_assignment_target().ok_or_else(|| c.unsupported(t.span(), "assignment target"))?, None),
                };
                let pre = c.pre_target(binding)?;
                c.emit(Insn::IteratorNext { iter });
                let v = c.temp();
                c.emit(Insn::Star(v));
                let name = match binding {
                    AssignmentTarget::AssignmentTargetIdentifier(id) => Some(id.name.as_str()),
                    _ => None,
                };
                let v2 = match default {
                    Some(d) => c.with_default(v, d, name)?,
                    None => v,
                };
                c.put_target(pre.as_ref(), binding, v2)?;
                c.release(v);
                if let Some(t) = &pre {
                    c.target_release(t);
                }
            }
            match &a.rest {
                Some(r) => {
                    let pre = c.pre_target(&r.target)?;
                    c.emit(Insn::IteratorRest { iter });
                    let v = c.temp();
                    c.emit(Insn::Star(v));
                    c.put_target(pre.as_ref(), &r.target, v)?;
                    c.release(v);
                    if let Some(t) = &pre {
                        c.target_release(t);
                    }
                }
                None => _ = c.emit(Insn::IteratorClose { iter }),
            }
            Ok(())
        })?;
        self.release(iter);
        Ok(())
    }

    /// 数组解构的元素：目标不是模式就先求值它的引用（对象与键进临时寄存器）；是模式给 `None`，取到值再展开。
    fn pre_target(&mut self, t: &'a AssignmentTarget<'a>) -> Result<Option<Target<'a>>, Error> {
        match t.as_simple_assignment_target() {
            Some(s) => Ok(Some(self.target(s)?)),
            None => Ok(None),
        }
    }

    /// 把 `v` 写进先求好的目标，或按嵌套模式展开。
    fn put_target(&mut self, pre: Option<&Target<'a>>, t: &'a AssignmentTarget<'a>, v: Reg) -> Result<(), Error> {
        match pre {
            Some(target) => {
                self.emit(Insn::Ldar(v));
                self.target_store(target);
                Ok(())
            }
            None => self.assign_target(t, v),
        }
    }

    fn assign_maybe_default(&mut self, e: &'a AssignmentTargetMaybeDefault<'a>, value: Reg) -> Result<(), Error> {
        match e {
            AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => {
                let name = match &d.binding {
                    AssignmentTarget::AssignmentTargetIdentifier(id) => Some(id.name.as_str()),
                    _ => None,
                };
                let v = self.with_default(value, &d.init, name)?;
                self.assign_target(&d.binding, v)?;
                self.release(v);
                Ok(())
            }
            t => self.assign_target(t.as_assignment_target().ok_or_else(|| self.unsupported(t.span(), "assignment target"))?, value),
        }
    }

    /// `yield* a`：把恢复方式与送进来的值原样转给内层迭代器，它走完时的值是表达式的值。
    fn yield_star(&mut self, a: &'a Expression<'a>, mode: Reg) -> Result<(), Error> {
        self.expr(a)?;
        let src = self.temp();
        self.emit(Insn::Star(src));
        let iter = self.iterate(src);
        let next = self.constant(Constant::Number(f64::from(Resume::Next)));
        self.emit(Insn::LdaConst(next));
        self.emit(Insn::Star(mode));
        self.emit(Insn::LdaUndefined);
        let result = self.temp();
        let top = self.here();
        self.emit(Insn::DelegateStep { iter, mode });
        self.emit(Insn::Star(result));
        let done = self.name("done");
        self.emit(Insn::GetNamed { obj: result, name: done });
        let finished = self.jump(Insn::JumpIfTrue);
        let value = self.name("value");
        self.emit(Insn::GetNamed { obj: result, name: value });
        self.emit(Insn::Yield { mode });
        self.emit(Insn::Jump(top));
        self.land(finished);
        self.emit(Insn::GetNamed { obj: result, name: value });
        self.emit(Insn::Star(result));
        self.on_completion(mode, f64::from(Resume::Return), |s| {
            s.emit(Insn::Ldar(result));
            s.ret();
            Ok(())
        })?;
        self.emit(Insn::Ldar(result));
        self.release(src);
        Ok(())
    }

    /// async 生成器里的 `yield* a`：async 迭代器协议，内层每步的结果先等；退回同步迭代器时值也等。
    /// `await` 恢复时会改写 `mode`，所以委托用的恢复方式另存一份。
    fn yield_star_async(&mut self, a: &'a Expression<'a>, mode: Reg) -> Result<(), Error> {
        self.expr(a)?;
        let iter = self.temps(4);
        self.emit(Insn::GetAsyncIterator { dst: iter });
        let next = self.constant(Constant::Number(f64::from(Resume::Next)));
        self.emit(Insn::LdaConst(next));
        self.emit(Insn::Star(mode));
        self.emit(Insn::LdaUndefined);
        let delegated = self.temp();
        let result = self.temp();
        let top = self.here();
        self.emit(Insn::DelegateStep { iter, mode });
        self.emit(Insn::Star(result));
        self.emit(Insn::Ldar(mode));
        self.emit(Insn::Star(delegated));
        self.emit(Insn::Ldar(result));
        self.emit(Insn::Await { mode });
        self.resume_dispatch(mode, false)?;
        self.emit(Insn::Star(result));
        let done = self.name("done");
        self.emit(Insn::GetNamed { obj: result, name: done });
        let finished = self.jump(Insn::JumpIfTrue);
        let value = self.name("value");
        self.emit(Insn::GetNamed { obj: result, name: value });
        self.emit(Insn::Star(result));
        self.emit(Insn::Ldar(Reg(iter.0 + 3)));
        let not_sync = self.jump(Insn::JumpIfFalse);
        self.emit(Insn::Ldar(result));
        self.emit(Insn::Await { mode });
        self.resume_dispatch(mode, false)?;
        self.emit(Insn::Star(result));
        self.land(not_sync);
        self.emit(Insn::Ldar(result));
        self.emit(Insn::Yield { mode });
        self.emit(Insn::Jump(top));
        self.land(finished);
        self.emit(Insn::GetNamed { obj: result, name: value });
        self.emit(Insn::Star(result));
        self.on_completion(delegated, f64::from(Resume::Return), |s| {
            s.emit(Insn::Ldar(result));
            s.ret();
            Ok(())
        })?;
        self.emit(Insn::Ldar(result));
        self.release(iter);
        Ok(())
    }

    /// 实参逐个求值放进 `first` 起的连续寄存器。
    fn arguments(&mut self, args: &'a [Argument<'a>], first: Reg) -> Result<(), Error> {
        for (i, a) in (0..).zip(args) {
            let e = a.as_expression().ok_or_else(|| self.unsupported(a.span(), "spread arguments"))?;
            self.expr(e)?;
            self.emit(Insn::Star(Reg(first.0 + i)));
        }
        Ok(())
    }

    /// `this` 读进一个临时寄存器（`super.x` 的接收者）。
    fn this_temp(&mut self) -> Reg {
        self.load(crate::scope::THIS);
        let t = self.temp();
        self.emit(Insn::Star(t));
        t
    }

    /// `delete x`：属性删掉给结果；删名字（非严格模式）给 `false`；别的表达式求完给 `true`。
    fn delete(&mut self, e: &'a Expression<'a>) -> Result<(), Error> {
        match e {
            Expression::StaticMemberExpression(m) => {
                self.expr(&m.object)?;
                let obj = self.temp();
                self.emit(Insn::Star(obj));
                let name = self.name(m.property.name.as_str());
                self.emit(Insn::DeleteNamed { obj, name });
                self.release(obj);
            }
            Expression::ComputedMemberExpression(m) => {
                self.expr(&m.object)?;
                let obj = self.temp();
                self.emit(Insn::Star(obj));
                self.expr(&m.expression)?;
                self.emit(Insn::DeleteKeyed { obj });
                self.release(obj);
            }
            Expression::Identifier(_) => _ = self.emit(Insn::LdaBool(false)),
            e => {
                self.expr(e)?;
                self.emit(Insn::LdaBool(true));
            }
        }
        Ok(())
    }

    fn object(&mut self, o: &'a ObjectExpression<'a>) -> Result<(), Error> {
        self.emit(Insn::CreateObject);
        let obj = self.temp();
        self.emit(Insn::Star(obj));
        for p in &o.properties {
            let p = match p {
                ObjectPropertyKind::ObjectProperty(p) => p,
                ObjectPropertyKind::SpreadProperty(sp) => {
                    self.expr(&sp.argument)?;
                    self.emit(Insn::CopyData { obj });
                    continue;
                }
            };
            let kind = match p.kind {
                PropertyKind_::Init => PropertyKind::Data,
                PropertyKind_::Get => PropertyKind::Getter,
                PropertyKind_::Set => PropertyKind::Setter,
            };
            let key = match self.static_key(&p.key, p.computed) {
                Some(k) => Ok(k?),
                None => {
                    let k = p.key.as_expression().ok_or_else(|| self.unsupported(p.span, "property key"))?;
                    self.expr(k)?;
                    let key = self.temp();
                    self.emit(Insn::Star(key));
                    Err(key)
                }
            };
            match (&p.value, p.method || kind != PropertyKind::Data) {
                (Expression::FunctionExpression(f), true) => {
                    let mkind = match p.kind {
                        PropertyKind_::Get => MethodDefinitionKind::Get,
                        PropertyKind_::Set => MethodDefinitionKind::Set,
                        PropertyKind_::Init => MethodDefinitionKind::Method,
                    };
                    let fname = self.method_name(&p.key, p.computed, mkind);
                    let n = self.method(fname, f, FunctionKind::Method)?;
                    self.emit(Insn::CreateMethod { func: n, home: obj });
                }
                (v, _) => match self.static_name(&p.key, p.computed) {
                    // `__proto__: function () {}` 不推断名字
                    Some(n) if n != "__proto__" || p.shorthand => self.expr_named(v, n)?,
                    _ => self.expr(v)?,
                },
            }
            match key {
                Ok(name) => _ = self.emit(Insn::DefineNamed { obj, name, kind, enumerable: true }),
                Err(key) => {
                    self.emit(Insn::DefineKeyed { obj, key, kind, enumerable: true });
                    self.release(key);
                }
            }
        }
        self.emit(Insn::Ldar(obj));
        self.release(obj);
        Ok(())
    }
}
