//! JS ⇒ V8 bytecode: expressions, after V8's `BytecodeGenerator`. Results are always left in acc.
use oxc_ast::ast::{
    Argument, ArrayAssignmentTarget, ArrayExpressionElement, ArrowFunctionBody, AssignmentTarget, AssignmentTargetMaybeDefault, AssignmentTargetProperty, ChainElement, ComputedMemberExpression, StaticMemberExpression, TaggedTemplateExpression, ArrowFunctionExpression, AssignmentExpression, AssignmentOperator, BinaryExpression, BinaryOperator, CallExpression, Expression, LogicalOperator, MemberExpression, MethodDefinitionKind, ObjectExpression, ObjectPropertyKind, PropertyKind as PropertyKind_, SimpleAssignmentTarget, TemplateElement, UnaryOperator, UpdateExpression,
    UpdateOperator,
};
use oxc_span::{GetSpan, Span};
use j8framework::{BinaryOp, CompareOp, Error};

use crate::builder::{Label, RegisterList};
use crate::compiler::{Body, Compiler, Flavor, Kind, Place};
use j8bytecode::{ArrayBoilerplate, Bytecode, Constant, CreateArrayLiteralFlags, CreateObjectLiteralFlags, LiteralFlags, LiteralValue, ObjectBoilerplate, Register, Runtime, TemplateObject, TypeOfLiteral};

/// V8's `JSRegExp::Flags` bits (`CreateRegExpLiteral`'s flags operand)
const REGEXP_FLAGS: [(char, u16); 9] = [('g', 1), ('i', 1 << 1), ('m', 1 << 2), ('y', 1 << 3), ('u', 1 << 4), ('s', 1 << 5), ('l', 1 << 6), ('d', 1 << 7), ('v', 1 << 8)];

/// Binary operator ⇒ bytecode operation.
#[derive(Clone, Copy)]
enum Operator {
    Binary(BinaryOp),
    Compare(CompareOp),
    InstanceOf,
    In,
}

/// The three kinds of assignment operator.
enum Assign {
    Plain,
    Binary(BinaryOp),
    /// `||=` / `&&=` / `??=`: the right side is not evaluated if the left side already satisfies the condition
    Logical(LogicalOperator),
}

/// An element of an array literal / argument list.
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

/// Assignment target: a name, or a property whose object (and key) are already evaluated.
enum Target<'a> {
    /// The name and its position in the source (to decide whether a TDZ check is needed)
    Name(&'a str, u32),
    Named { obj: Register, name: &'a str },
    Keyed { obj: Register, key: Register },
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
            AssignmentOperator::LogicalOr => Assign::Logical(LogicalOperator::Or),
            AssignmentOperator::LogicalAnd => Assign::Logical(LogicalOperator::And),
            AssignmentOperator::LogicalNullish => Assign::Logical(LogicalOperator::Coalesce),
        }
    }

    pub fn string_const(&mut self, s: Option<&str>, span: Span) -> Result<usize, Error> {
        let s = s.ok_or_else(|| self.unsupported(span, "lone surrogates"))?;
        Ok(self.constant(Constant::String(s.into())))
    }

    /// A segment of a template literal (text after escape processing).
    fn quasi(&mut self, q: &TemplateElement<'a>) -> Result<usize, Error> {
        let cooked = match q.value.cooked {
            Some(c) => c.as_str(),
            None => None,
        };
        self.string_const(cooked, q.span)
    }

    /// A literal a boilerplate can hold (`None`: computed at run time)
    fn literal_value(e: &Expression<'_>) -> Option<LiteralValue> {
        match e {
            Expression::NumericLiteral(n) => Some(LiteralValue::Number(n.value)),
            Expression::StringLiteral(s) => s.value.as_str().map(|v| LiteralValue::String(v.into())),
            Expression::BooleanLiteral(b) => Some(LiteralValue::Bool(b.value)),
            Expression::NullLiteral(_) => Some(LiteralValue::Null),
            _ => None,
        }
    }

    /// Jumps by the left side of a logical operator: `||` / `&&` skip the right side on a truthy / falsy left side,
    /// `??` on a left side that isn't nullish. The label is bound by the caller after the right side.
    fn logical_skip(&mut self, op: LogicalOperator, end: &mut Label) {
        match op {
            LogicalOperator::Or => self.b().jump(Bytecode::JumpIfToBooleanTrue, end),
            LogicalOperator::And => self.b().jump(Bytecode::JumpIfToBooleanFalse, end),
            LogicalOperator::Coalesce => {
                let mut right = Label::default();
                self.b().jump(Bytecode::JumpIfUndefinedOrNull, &mut right);
                self.b().jump(Bytecode::Jump, end);
                self.b().bind(&mut right);
            }
        }
    }

    #[expect(clippy::too_many_lines, reason = "one arm per expression kind; splitting it would hurt readability")]
    pub fn expr(&mut self, e: &'a Expression<'a>) -> Result<(), Error> {
        match e {
            Expression::NumericLiteral(n) => self.b().load_number(n.value),
            Expression::StringLiteral(s) => {
                let k = self.string_const(s.value.as_str(), s.span)?;
                self.b().load_constant(k);
            }
            Expression::BooleanLiteral(b) => self.b().load_bool(b.value),
            Expression::BigIntLiteral(b) => {
                let k = self.constant(Constant::BigInt((&*b.value).into()));
                self.b().load_constant(k);
            }
            Expression::RegExpLiteral(r) => {
                let pattern = self.name(&r.regex.pattern.text);
                let text = r.regex.flags.to_string();
                let flags = REGEXP_FLAGS.iter().filter(|(c, _)| text.contains(*c)).fold(0, |a, (_, b)| a | b);
                self.b().create_reg_exp_literal(pattern, flags);
            }
            Expression::NullLiteral(_) => self.b().load_null(),
            Expression::TemplateLiteral(t) => {
                // q0 + e0 + q1 + …: each substitution is converted with ToString, then concatenated
                let first = t.quasis.first().expect("a template has a first quasi");
                let k = self.quasi(first)?;
                self.b().load_constant(k);
                let acc = self.temp();
                for (e, q) in t.expressions.iter().zip(t.quasis.iter().skip(1)) {
                    self.b().star(acc);
                    self.expr(e)?;
                    self.b().plain(Bytecode::ToString);
                    self.b().binary(BinaryOp::Add, acc);
                    self.b().star(acc);
                    let k = self.quasi(q)?;
                    self.b().load_constant(k);
                    self.b().binary(BinaryOp::Add, acc);
                }
                self.release(acc);
            }
            Expression::Identifier(id) => self.load_at(id.name.as_str(), id.span.start),
            Expression::ThisExpression(t) => self.load_at(crate::scope::THIS, t.span.start),
            Expression::ParenthesizedExpression(p) => self.expr(&p.expression)?,
            Expression::FunctionExpression(f) => {
                let n = self.plain_function(f, true, "")?;
                self.closure(n);
            }
            Expression::ArrowFunctionExpression(a) => self.arrow(a, "")?,
            Expression::ClassExpression(c) => self.class(c, "")?,
            Expression::NewTarget(t) => self.load_at(crate::scope::NEW_TARGET, t.span.start),
            Expression::TaggedTemplateExpression(t) => self.tagged_template(t)?,
            Expression::NewExpression(n) => {
                let callee = self.temp();
                self.expr(&n.callee)?;
                self.b().star(callee);
                let (args, spread) = self.argument_list(&n.arguments, None)?;
                self.b().ldar(callee);
                if spread {
                    self.b().construct_with_spread(callee, args);
                } else {
                    self.b().construct(callee, args);
                }
                self.release(callee);
            }
            Expression::UnaryExpression(u) => {
                match u.operator {
                    UnaryOperator::Delete => return self.delete(&u.argument),
                    UnaryOperator::Typeof => {
                        match &u.argument {
                            // `typeof` of an undeclared name does not throw
                            Expression::Identifier(id) => self.load_typeof(id.name.as_str()),
                            a => self.expr(a)?,
                        }
                        self.b().unary(Bytecode::TypeOf);
                    }
                    UnaryOperator::Void => {
                        self.effect(&u.argument)?;
                        self.b().load_undefined();
                    }
                    UnaryOperator::UnaryNegation => {
                        self.expr(&u.argument)?;
                        self.b().unary(Bytecode::Negate);
                    }
                    UnaryOperator::UnaryPlus => {
                        self.expr(&u.argument)?;
                        self.b().to_number();
                    }
                    UnaryOperator::BitwiseNot => {
                        self.expr(&u.argument)?;
                        self.b().unary(Bytecode::BitwiseNot);
                    }
                    UnaryOperator::LogicalNot => {
                        self.expr(&u.argument)?;
                        self.b().plain(Bytecode::ToBooleanLogicalNot);
                    }
                }
            }
            Expression::BinaryExpression(b) => self.binary(b)?,
            Expression::LogicalExpression(l) => {
                self.expr(&l.left)?;
                let mut end = Label::default();
                self.logical_skip(l.operator, &mut end);
                self.expr(&l.right)?;
                self.b().bind(&mut end);
            }
            Expression::ConditionalExpression(c) => {
                self.expr(&c.test)?;
                let mut other = Label::default();
                self.b().jump(Bytecode::JumpIfToBooleanFalse, &mut other);
                self.expr(&c.consequent)?;
                let mut end = Label::default();
                self.b().jump(Bytecode::Jump, &mut end);
                self.b().bind(&mut other);
                self.expr(&c.alternate)?;
                self.b().bind(&mut end);
            }
            Expression::SequenceExpression(s) => {
                let last = s.expressions.len().saturating_sub(1);
                for (i, e) in s.expressions.iter().enumerate() {
                    if i == last { self.expr(e)? } else { self.effect(e)? }
                }
            }
            Expression::AssignmentExpression(a) => self.assignment(a)?,
            Expression::UpdateExpression(u) => self.update(u)?,
            Expression::CallExpression(c) => self.call(c)?,
            Expression::StaticMemberExpression(m) if matches!(m.object, Expression::Super(_)) => return Err(self.unsupported(m.span, "super")),
            Expression::ComputedMemberExpression(m) if matches!(m.object, Expression::Super(_)) => return Err(self.unsupported(m.span, "super")),
            Expression::StaticMemberExpression(m) => self.static_member(m)?,
            Expression::ComputedMemberExpression(m) => self.computed_member(m)?,
            Expression::ChainExpression(c) => {
                self.chains.push(Label::default());
                match &c.expression {
                    ChainElement::CallExpression(call) => self.call(call)?,
                    ChainElement::StaticMemberExpression(m) => self.static_member(m)?,
                    ChainElement::ComputedMemberExpression(m) => self.computed_member(m)?,
                    e => return Err(self.unsupported(e.span(), "optional chain element")),
                }
                let mut nullish = self.chains.pop().expect("this chain");
                let mut end = Label::default();
                self.b().jump(Bytecode::Jump, &mut end);
                self.b().bind(&mut nullish);
                self.b().load_undefined();
                self.b().bind(&mut end);
            }
            Expression::ObjectExpression(o) => self.object(o)?,
            Expression::ArrayExpression(a) => self.array(&a.elements, a.span)?,
            e => return Err(self.unsupported(e.span(), "expression")),
        }
        Ok(())
    }

    /// Anonymous functions / arrow functions / anonymous classes get an inferred name per the spec (NamedEvaluation);
    /// everything else is evaluated as usual.
    pub fn expr_named(&mut self, e: &'a Expression<'a>, name: &'a str) -> Result<(), Error> {
        match e {
            Expression::FunctionExpression(f) if f.id.is_none() => {
                let n = self.plain_function(f, true, name)?;
                self.closure(n);
                Ok(())
            }
            Expression::ArrowFunctionExpression(a) => self.arrow(a, name),
            Expression::ClassExpression(c) if c.id.is_none() => self.class(c, name),
            Expression::ParenthesizedExpression(p) => self.expr_named(&p.expression, name),
            e => self.expr(e),
        }
    }

    /// Tagged template: `tag(templateObject, ...substitutions)`; the template object is cached per call site
    /// (`GetTemplateObject` with a template description constant).
    fn tagged_template(&mut self, t: &'a TaggedTemplateExpression<'a>) -> Result<(), Error> {
        let subs = &t.quasi.expressions;
        let argc = u32::try_from(subs.len() + 1).map_err(|_| self.unsupported(t.span, "too many substitutions"))?;
        let callee = self.temp();
        let receiver = match &t.tag {
            Expression::StaticMemberExpression(m) => {
                let list = self.b().new_register_list(1 + argc);
                self.expr(&m.object)?;
                self.b().star(list.first);
                self.b().get_named(list.first, m.property.name.as_str());
                Some(list)
            }
            Expression::ComputedMemberExpression(m) => {
                let list = self.b().new_register_list(1 + argc);
                self.expr(&m.object)?;
                self.b().star(list.first);
                self.expr(&m.expression)?;
                self.b().get_keyed(list.first);
                Some(list)
            }
            tag => {
                self.expr(tag)?;
                None
            }
        };
        self.b().star(callee);
        let args = match receiver {
            Some(l) => RegisterList { first: l.at(1), count: argc },
            None => self.b().new_register_list(argc),
        };
        let mut cooked = Vec::new();
        let mut raw = Vec::new();
        for q in &t.quasi.quasis {
            cooked.push(q.value.cooked.and_then(|c| c.as_str().map(Into::into)));
            raw.push((&*q.value.raw).into());
        }
        let k = self.constant(Constant::Template(TemplateObject { cooked, raw }));
        self.b().get_template_object(k);
        self.b().star(args.first);
        for (i, e) in (1u32..).zip(subs) {
            self.expr(e)?;
            self.b().star(args.at(i));
        }
        match receiver {
            Some(l) => self.b().call_property(callee, RegisterList { first: l.first, count: argc + 1 }),
            None => self.b().call_undefined_receiver(callee, args),
        }
        self.release(callee);
        Ok(())
    }

    fn arrow(&mut self, a: &'a ArrowFunctionExpression<'a>, name: &str) -> Result<(), Error> {
        let body = match &a.body {
            ArrowFunctionBody::FunctionBody(b) => Body::Statements(&b.statements),
            e => Body::Expression(e.as_expression().expect("an arrow's body is a block or an expression")),
        };
        let strict = match &a.body {
            ArrowFunctionBody::FunctionBody(b) => Flavor::use_strict(&b.directives),
            _ => false,
        };
        let n = self.function(name, Flavor { kind: Kind::Arrow, generator: false, asynchronous: a.r#async, strict, statik: false }, None, None, Some(&a.params), body)?;
        self.closure(n);
        Ok(())
    }

    /// A Smi literal operand for the `*Smi` bytecodes (V8 uses them when the right side is a small integer literal)
    fn smi_literal(e: &Expression<'_>) -> Option<i32> {
        let Expression::NumericLiteral(n) = e else { return None };
        crate::builder::Builder::is_smi(n.value)
    }

    fn binary(&mut self, b: &'a BinaryExpression<'a>) -> Result<(), Error> {
        let op = Self::operator(b.operator);
        if let (Operator::Binary(op), Some(n)) = (op, Self::smi_literal(&b.right)) {
            self.expr(&b.left)?;
            self.b().binary_smi(op, n);
            return Ok(());
        }
        // The left side is a variable in a register and the right side cannot change it: use its register directly as
        // the left operand instead of copying it into a temporary register first
        if let Expression::Identifier(id) = &b.left
            && Self::cannot_assign(&b.right)
            && let Some(l) = self.register_of(id.name.as_str(), id.span.start)
        {
            self.expr(&b.right)?;
            self.binary_op(op, l);
            return Ok(());
        }
        self.expr(&b.left)?;
        let l = self.temp();
        self.b().star(l);
        self.expr(&b.right)?;
        self.binary_op(op, l);
        self.release(l);
        Ok(())
    }

    /// acc = `l` op acc
    fn binary_op(&mut self, op: Operator, l: Register) {
        match op {
            Operator::Binary(op) => self.b().binary(op, l),
            Operator::Compare(op) => self.b().compare(op, l),
            Operator::InstanceOf => self.b().test_instance_of(l),
            Operator::In => self.b().test_in(l),
        }
    }

    /// Evaluating `e` cannot assign to this function's register variables: it consists only of literals, names, `this`,
    /// static member reads, and unary / binary / logical / conditional operations (user code called by these operations
    /// lives in other functions and cannot touch the variables in this function's registers).
    fn cannot_assign(e: &Expression<'_>) -> bool {
        match e {
            Expression::NumericLiteral(_) | Expression::StringLiteral(_) | Expression::BooleanLiteral(_) | Expression::NullLiteral(_) | Expression::BigIntLiteral(_) | Expression::Identifier(_) | Expression::ThisExpression(_) => true,
            Expression::ParenthesizedExpression(p) => Self::cannot_assign(&p.expression),
            Expression::UnaryExpression(u) => u.operator != UnaryOperator::Delete && Self::cannot_assign(&u.argument),
            Expression::BinaryExpression(b) => Self::cannot_assign(&b.left) && Self::cannot_assign(&b.right),
            Expression::LogicalExpression(l) => Self::cannot_assign(&l.left) && Self::cannot_assign(&l.right),
            Expression::ConditionalExpression(c) => Self::cannot_assign(&c.test) && Self::cannot_assign(&c.consequent) && Self::cannot_assign(&c.alternate),
            Expression::StaticMemberExpression(m) => Self::cannot_assign(&m.object),
            _ => false,
        }
    }

    /// Evaluates the object (and key) of an assignment target into temporary registers.
    fn target(&mut self, t: &'a SimpleAssignmentTarget<'a>) -> Result<Target<'a>, Error> {
        if let SimpleAssignmentTarget::AssignmentTargetIdentifier(id) = t {
            return Ok(Target::Name(id.name.as_str(), id.span.start));
        }
        match t.as_member_expression() {
            Some(MemberExpression::StaticMemberExpression(m)) if !matches!(m.object, Expression::Super(_)) => {
                self.expr(&m.object)?;
                let obj = self.temp();
                self.b().star(obj);
                Ok(Target::Named { obj, name: m.property.name.as_str() })
            }
            Some(MemberExpression::ComputedMemberExpression(m)) if !matches!(m.object, Expression::Super(_)) => {
                self.expr(&m.object)?;
                let obj = self.temp();
                self.b().star(obj);
                self.expr(&m.expression)?;
                let key = self.temp();
                self.b().star(key);
                Ok(Target::Keyed { obj, key })
            }
            _ => Err(self.unsupported(t.span(), "assignment target")),
        }
    }

    fn target_load(&mut self, t: &Target<'a>) {
        match *t {
            Target::Name(n, pos) => self.load_at(n, pos),
            Target::Named { obj, name } => self.b().get_named(obj, name),
            Target::Keyed { obj, key } => {
                self.b().ldar(key);
                self.b().get_keyed(obj);
            }
        }
    }

    fn target_store(&mut self, t: &Target<'a>) {
        let strict = self.strict_here();
        match *t {
            Target::Name(n, pos) => self.store_at(n, false, pos),
            Target::Named { obj, name } => self.b().set_named(obj, name, strict),
            Target::Keyed { obj, key } => self.b().set_keyed(obj, key, strict),
        }
    }

    fn target_release(&mut self, t: &Target<'a>) {
        match *t {
            Target::Name(..) => {}
            Target::Named { obj, .. } | Target::Keyed { obj, .. } => self.release(obj),
        }
    }

    fn assignment(&mut self, a: &'a AssignmentExpression<'a>) -> Result<(), Error> {
        let Some(t) = a.left.as_simple_assignment_target() else {
            self.expr(&a.right)?;
            let v = self.temp();
            self.b().star(v);
            self.assign_target(&a.left, v)?;
            self.b().ldar(v);
            self.release(v);
            return Ok(());
        };
        // `o.x = v` / `o[k] = v`: the object is in a register and the right side (and the key) cannot change it, so
        // write directly without copying the object first
        if a.operator == AssignmentOperator::Assign
            && Self::cannot_assign(&a.right)
            && let Some(m) = t.as_member_expression()
        {
            let strict = self.strict_here();
            match m {
                MemberExpression::StaticMemberExpression(m) if let Some(obj) = self.in_register(&m.object) => {
                    self.expr(&a.right)?;
                    self.b().set_named(obj, m.property.name.as_str(), strict);
                    return Ok(());
                }
                MemberExpression::ComputedMemberExpression(m) if Self::cannot_assign(&m.expression) && let Some(obj) = self.in_register(&m.object) => {
                    self.expr(&m.expression)?;
                    let key = self.temp();
                    self.b().star(key);
                    self.expr(&a.right)?;
                    self.b().set_keyed(obj, key, strict);
                    self.release(key);
                    return Ok(());
                }
                _ => {}
            }
        }
        let t = self.target(t)?;
        match Self::assign_kind(a.operator) {
            Assign::Plain => {
                match &t {
                    Target::Name(n, _) => self.expr_named(&a.right, n)?,
                    _ => self.expr(&a.right)?,
                }
                self.target_store(&t);
            }
            Assign::Binary(op) => {
                self.target_load(&t);
                if let Some(n) = Self::smi_literal(&a.right) {
                    self.b().binary_smi(op, n);
                } else {
                    let old = self.temp();
                    self.b().star(old);
                    self.expr(&a.right)?;
                    self.b().binary(op, old);
                    self.release(old);
                }
                self.target_store(&t);
            }
            Assign::Logical(op) => {
                self.target_load(&t);
                let mut end = Label::default();
                self.logical_skip(op, &mut end);
                match &t {
                    Target::Name(n, _) => self.expr_named(&a.right, n)?,
                    _ => self.expr(&a.right)?,
                }
                self.target_store(&t);
                self.b().bind(&mut end);
            }
        }
        self.target_release(&t);
        Ok(())
    }

    /// Evaluates `e` with the result unused (expression statements in functions, the update part of `for`): postfix
    /// `++` / `--` does not keep the old value.
    pub fn effect(&mut self, e: &'a Expression<'a>) -> Result<(), Error> {
        match e {
            Expression::UpdateExpression(u) => self.update_as(u, true),
            Expression::ParenthesizedExpression(p) => self.effect(&p.expression),
            _ => self.expr(e),
        }
    }

    fn update(&mut self, u: &'a UpdateExpression<'a>) -> Result<(), Error> {
        self.update_as(u, false)
    }

    /// `++` / `--` (V8's `VisitCountOperation`: postfix with a used value converts with `ToNumeric` and keeps the old
    /// value); with `discard` the result is unused.
    fn update_as(&mut self, u: &'a UpdateExpression<'a>, discard: bool) -> Result<(), Error> {
        let t = self.target(&u.argument)?;
        self.target_load(&t);
        let old = (!u.prefix && !discard).then(|| self.temp());
        if let Some(r) = old {
            self.b().to_numeric();
            self.b().star(r);
        }
        self.b().unary(match u.operator {
            UpdateOperator::Increment => Bytecode::Inc,
            UpdateOperator::Decrement => Bytecode::Dec,
        });
        self.target_store(&t);
        if let Some(r) = old {
            self.b().ldar(r);
            self.release(r);
        }
        self.target_release(&t);
        Ok(())
    }

    /// The arguments into a new register list (after `receiver`'s register when given, which then heads the list); the
    /// flag tells whether the last argument is a spread (`CallWithSpread` / `ConstructWithSpread`)
    fn argument_list(&mut self, args: &'a [Argument<'a>], receiver: Option<Register>) -> Result<(RegisterList, bool), Error> {
        let spread_at = args.iter().position(|a| matches!(a, Argument::SpreadElement(_)));
        if spread_at.is_some_and(|i| i + 1 != args.len()) {
            return Err(self.unsupported(args[spread_at.unwrap_or(0)].span(), "a spread argument before the last"));
        }
        let n = u32::try_from(args.len()).map_err(|_| self.unsupported(Span::default(), "too many arguments"))?;
        let list = self.b().new_register_list(n);
        for (i, a) in (0u32..).zip(args) {
            let e = match a {
                Argument::SpreadElement(s) => &s.argument,
                a => a.as_expression().expect("the rest are expressions"),
            };
            self.expr(e)?;
            self.b().star(list.at(i));
        }
        let out = match receiver {
            Some(r) => RegisterList { first: r, count: n + 1 },
            None => list,
        };
        Ok((out, spread_at.is_some()))
    }

    /// Call (V8's `VisitCall`): the callee in a register; a property call's receiver heads the argument list
    /// (`CallProperty*`), a plain call has an undefined receiver (`CallUndefinedReceiver*`); a possible direct eval
    /// first resolves its callee with `ResolvePossiblyDirectEval`.
    fn call(&mut self, c: &'a CallExpression<'a>) -> Result<(), Error> {
        if let Expression::Super(_) = &c.callee {
            return Err(self.unsupported(c.span, "super()"));
        }
        let callee = self.temp();
        let receiver = match &c.callee {
            Expression::StaticMemberExpression(m) if !matches!(m.object, Expression::Super(_)) => {
                let r = self.temp();
                self.expr(&m.object)?;
                self.b().star(r);
                self.optional(m.optional, m.span)?;
                self.b().get_named(r, m.property.name.as_str());
                Some(r)
            }
            Expression::ComputedMemberExpression(m) if !matches!(m.object, Expression::Super(_)) => {
                let r = self.temp();
                self.expr(&m.object)?;
                self.b().star(r);
                self.optional(m.optional, m.span)?;
                self.expr(&m.expression)?;
                self.b().get_keyed(r);
                Some(r)
            }
            Expression::StaticMemberExpression(m) => return Err(self.unsupported(m.span, "super")),
            Expression::ComputedMemberExpression(m) => return Err(self.unsupported(m.span, "super")),
            Expression::PrivateFieldExpression(m) => return Err(self.unsupported(m.span, "private names")),
            f => {
                self.expr(f)?;
                None
            }
        };
        self.optional(c.optional, c.span)?;
        self.b().star(callee);
        let (args, spread) = self.argument_list(&c.arguments, receiver)?;
        let eval = receiver.is_none() && matches!(&c.callee, Expression::Identifier(id) if id.name == "eval") && !spread;
        if eval {
            self.resolve_possibly_direct_eval(callee, args, c.span);
        }
        match (receiver, spread) {
            (_, true) => {
                let list = match receiver {
                    Some(r) => RegisterList { first: r, count: args.count },
                    None => {
                        // CallWithSpread takes a receiver first: undefined
                        args
                    }
                };
                self.b().call_with_spread(callee, list);
            }
            (Some(_), false) => self.b().call_property(callee, args),
            (None, false) => self.b().call_undefined_receiver(callee, args),
        }
        self.release(callee);
        Ok(())
    }

    /// `ResolvePossiblyDirectEval(callee, source, function, language mode, scope position, eval position)`: the callee
    /// stays as is unless it is the real `eval`, which becomes the compiled eval code
    fn resolve_possibly_direct_eval(&mut self, callee: Register, args: RegisterList, span: Span) {
        let list = self.b().new_register_list(6);
        self.b().mov(callee, list.first);
        if args.count > 0 {
            self.b().mov(args.first, list.at(1));
        } else {
            self.b().load_undefined();
            self.b().star(list.at(1));
        }
        self.b().mov(Register::function_closure(), list.at(2));
        let strict = self.strict_here();
        self.b().load_smi(i32::from(strict));
        self.b().star(list.at(3));
        self.b().load_smi(0);
        self.b().star(list.at(4));
        self.b().load_smi(i32::try_from(span.start).unwrap_or(i32::MAX));
        self.b().star(list.at(5));
        self.b().call_runtime(Runtime::ResolvePossiblyDirectEval, list);
        self.b().star(callee);
        self.release(list.first);
    }

    /// If `e` is a variable in this function's registers or `this` (no TDZ check needed here), gives that register, so
    /// reading it needs no copy into a temporary register.
    fn in_register(&self, e: &'a Expression<'a>) -> Option<Register> {
        match e {
            Expression::Identifier(id) => self.register_of(id.name.as_str(), id.span.start),
            Expression::ThisExpression(t) => self.register_of(crate::scope::THIS, t.span.start),
            Expression::ParenthesizedExpression(p) => self.in_register(&p.expression),
            _ => None,
        }
    }

    fn static_member(&mut self, m: &'a StaticMemberExpression<'a>) -> Result<(), Error> {
        if !m.optional
            && let Some(obj) = self.in_register(&m.object)
        {
            self.b().get_named(obj, m.property.name.as_str());
            return Ok(());
        }
        self.expr(&m.object)?;
        self.optional(m.optional, m.span)?;
        let obj = self.temp();
        self.b().star(obj);
        self.b().get_named(obj, m.property.name.as_str());
        self.release(obj);
        Ok(())
    }

    fn computed_member(&mut self, m: &'a ComputedMemberExpression<'a>) -> Result<(), Error> {
        // The object is in a register and the key cannot change it: evaluate the key first and use the object's
        // register directly
        if !m.optional
            && Self::cannot_assign(&m.expression)
            && let Some(obj) = self.in_register(&m.object)
        {
            self.expr(&m.expression)?;
            self.b().get_keyed(obj);
            return Ok(());
        }
        self.expr(&m.object)?;
        self.optional(m.optional, m.span)?;
        let obj = self.temp();
        self.b().star(obj);
        self.expr(&m.expression)?;
        self.b().get_keyed(obj);
        self.release(obj);
        Ok(())
    }

    /// `?.`: if acc is null / undefined, jump to the end of the current chain.
    fn optional(&mut self, optional: bool, span: Span) -> Result<(), Error> {
        if optional {
            let Some(mut l) = self.chains.pop() else { return Err(self.unsupported(span, "`?.` outside an optional chain")) };
            self.b().jump(Bytecode::JumpIfUndefinedOrNull, &mut l);
            self.chains.push(l);
        }
        Ok(())
    }

    /// An array literal (V8's `BuildCreateArrayLiteral`): a boilerplate with the constant elements, then the computed
    /// ones stored by index; with spreads / holes after a spread, built element by element from an empty array.
    fn array(&mut self, elements: &'a [ArrayExpressionElement<'a>], span: Span) -> Result<(), Error> {
        if elements.iter().any(|e| matches!(e, ArrayExpressionElement::SpreadElement(_))) {
            let arr = self.spread_array(elements.iter().map(Element::from))?;
            self.b().ldar(arr);
            self.release(arr);
            return Ok(());
        }
        if elements.is_empty() {
            self.b().create_empty_array_literal();
            return Ok(());
        }
        let boilerplate: Vec<Option<LiteralValue>> = elements
            .iter()
            .map(|e| match e {
                ArrayExpressionElement::Elision(_) => Some(LiteralValue::Hole),
                e => e.as_expression().and_then(Self::literal_value),
            })
            .collect();
        let shallow = boilerplate.iter().all(Option::is_some);
        let k = self.constant(Constant::Array(ArrayBoilerplate { elements: boilerplate.clone() }));
        let flags = CreateArrayLiteralFlags::encode(shallow, if shallow { LiteralFlags::IS_SHALLOW } else { LiteralFlags::NO_FLAGS });
        self.b().create_array_literal(k, flags);
        if shallow {
            return Ok(());
        }
        let arr = self.temp();
        let index = self.temp();
        self.b().star(arr);
        for (i, (e, b)) in elements.iter().zip(&boilerplate).enumerate() {
            if b.is_some() {
                continue;
            }
            let e = e.as_expression().ok_or_else(|| self.unsupported(span, "array element"))?;
            self.b().load_smi(i32::try_from(i).unwrap_or(i32::MAX));
            self.b().star(index);
            self.expr(e)?;
            self.b().store_in_array_literal(arr, index);
        }
        self.b().ldar(arr);
        self.release(arr);
        Ok(())
    }

    /// Builds a sequence of elements with spreads / holes into an array in a new temporary register (the caller frees
    /// it): elements are stored by index (`StaInArrayLiteral`), spreads iterated, the length set at the end.
    fn spread_array(&mut self, elements: impl Iterator<Item = Element<'a>>) -> Result<Register, Error> {
        let arr = self.temp();
        let index = self.temp();
        self.b().create_empty_array_literal();
        self.b().star(arr);
        self.b().load_smi(0);
        self.b().star(index);
        let mut hole_last = false;
        for e in elements {
            hole_last = false;
            match e {
                Element::Value(v) => {
                    self.expr(v)?;
                    self.b().store_in_array_literal(arr, index);
                    self.b().ldar(index);
                    self.b().unary(Bytecode::Inc);
                    self.b().star(index);
                }
                Element::Spread(v) => {
                    self.expr(v)?;
                    let src = self.temp();
                    self.b().star(src);
                    let iter = self.iterate(src);
                    let header = self.b().loop_header();
                    let mut done = Label::default();
                    self.iterator_next(iter);
                    let v = self.temp();
                    self.b().star(v);
                    self.b().ldar(Register(iter.0 + 2));
                    self.b().jump(Bytecode::JumpIfTrue, &mut done);
                    self.b().ldar(v);
                    self.b().store_in_array_literal(arr, index);
                    self.b().ldar(index);
                    self.b().unary(Bytecode::Inc);
                    self.b().star(index);
                    let depth = self.frame_loop_depth();
                    self.b().jump_loop(header, depth);
                    self.b().bind(&mut done);
                    self.release(src);
                }
                Element::Hole => {
                    hole_last = true;
                    self.b().ldar(index);
                    self.b().unary(Bytecode::Inc);
                    self.b().star(index);
                }
            }
        }
        if hole_last {
            let strict = self.strict_here();
            self.b().ldar(index);
            self.b().set_named(arr, "length", strict);
        }
        self.release(index);
        Ok(arr)
    }

    /// Destructuring assignment: stores the value in register `value` according to the assignment target pattern.
    pub fn assign_target(&mut self, t: &'a AssignmentTarget<'a>, value: Register) -> Result<(), Error> {
        match t {
            AssignmentTarget::ArrayAssignmentTarget(a) => self.assign_array(a, value)?,
            AssignmentTarget::ObjectAssignmentTarget(o) => {
                if o.properties.is_empty() {
                    self.require_object(value);
                }
                if o.rest.is_some() {
                    return Err(self.unsupported(o.span, "a rest element in an object assignment pattern"));
                }
                for p in &o.properties {
                    match p {
                        AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(id) => {
                            let name = id.binding.name.as_str();
                            self.b().get_named(value, name);
                            let v = self.temp();
                            self.b().star(v);
                            let v2 = match &id.init {
                                Some(d) => self.with_default(v, d, Some(name))?,
                                None => v,
                            };
                            self.b().ldar(v2);
                            self.store(name, false);
                            self.release(v);
                        }
                        AssignmentTargetProperty::AssignmentTargetPropertyProperty(pp) => {
                            let k = self.temp();
                            self.property_key_into(&pp.name, pp.computed, k)?;
                            self.b().ldar(k);
                            self.b().get_keyed(value);
                            let v = self.temp();
                            self.b().star(v);
                            self.assign_maybe_default(&pp.binding, v)?;
                            self.release(k);
                        }
                    }
                }
            }
            t => {
                let s = t.as_simple_assignment_target().ok_or_else(|| self.unsupported(t.span(), "assignment target"))?;
                let target = self.target(s)?;
                self.b().ldar(value);
                self.target_store(&target);
                self.target_release(&target);
            }
        }
        Ok(())
    }

    /// Array destructuring assignment, in spec order: for each element, evaluate the target reference first, then step
    /// the iterator; on a throw midway, close the iterator first.
    fn assign_array(&mut self, a: &'a ArrayAssignmentTarget<'a>, value: Register) -> Result<(), Error> {
        let iter = self.iterate(value);
        self.closing_on_throw(iter, |c| {
            for e in &a.elements {
                let Some(e) = e else {
                    c.iterator_next(iter);
                    continue;
                };
                let (binding, default) = match e {
                    AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) => (&d.binding, Some(&d.init)),
                    t => (t.as_assignment_target().ok_or_else(|| c.unsupported(t.span(), "assignment target"))?, None),
                };
                let pre = c.pre_target(binding)?;
                c.iterator_next(iter);
                let v = c.temp();
                c.b().star(v);
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
                    c.iterator_rest(iter);
                    let v = c.temp();
                    c.b().star(v);
                    c.put_target(pre.as_ref(), &r.target, v)?;
                    c.release(v);
                    if let Some(t) = &pre {
                        c.target_release(t);
                    }
                }
                None => c.iterator_close(iter),
            }
            Ok(())
        })?;
        self.release(iter);
        Ok(())
    }

    /// An element of array destructuring: if the target is not a pattern, evaluate its reference first (object and key
    /// go into temporary registers); for a pattern gives `None`, expanded once the value is available.
    fn pre_target(&mut self, t: &'a AssignmentTarget<'a>) -> Result<Option<Target<'a>>, Error> {
        match t.as_simple_assignment_target() {
            Some(s) => Ok(Some(self.target(s)?)),
            None => Ok(None),
        }
    }

    /// Writes `v` into the previously evaluated target, or expands it per the nested pattern.
    fn put_target(&mut self, pre: Option<&Target<'a>>, t: &'a AssignmentTarget<'a>, v: Register) -> Result<(), Error> {
        match pre {
            Some(target) => {
                self.b().ldar(v);
                self.target_store(target);
                Ok(())
            }
            None => self.assign_target(t, v),
        }
    }

    fn assign_maybe_default(&mut self, e: &'a AssignmentTargetMaybeDefault<'a>, value: Register) -> Result<(), Error> {
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

    /// `delete x` (V8's `VisitDelete`): a property delete gives the result (key in acc); deleting an unresolved name
    /// (sloppy mode) goes through `DeleteLookupSlot`, a declared variable gives `false`; any other expression is
    /// evaluated and gives `true`.
    fn delete(&mut self, e: &'a Expression<'a>) -> Result<(), Error> {
        let strict = self.strict_here();
        match e {
            Expression::StaticMemberExpression(m) if !matches!(m.object, Expression::Super(_)) => {
                self.expr(&m.object)?;
                let obj = self.temp();
                self.b().star(obj);
                self.b().load_string(m.property.name.as_str());
                self.b().delete_property(obj, strict);
                self.release(obj);
            }
            Expression::ComputedMemberExpression(m) if !matches!(m.object, Expression::Super(_)) => {
                self.expr(&m.object)?;
                let obj = self.temp();
                self.b().star(obj);
                self.expr(&m.expression)?;
                self.b().delete_property(obj, strict);
                self.release(obj);
            }
            Expression::Identifier(id) => match self.lookup(id.name.as_str()).0 {
                Place::Global(_) | Place::Lookup(_) => {
                    let r = self.temp();
                    self.b().load_string(id.name.as_str());
                    self.b().star(r);
                    self.b().call_runtime(Runtime::DeleteLookupSlot, RegisterList { first: r, count: 1 });
                    self.release(r);
                }
                _ => self.b().load_bool(false),
            },
            e => {
                self.effect(e)?;
                self.b().load_bool(true);
            }
        }
        Ok(())
    }

    /// An object literal (V8's `VisitObjectLiteral`): the boilerplate holds the constant-named data properties before
    /// the first computed key or spread (values known at compile time filled in); the rest are defined in order after
    /// creating it.
    fn object(&mut self, o: &'a ObjectExpression<'a>) -> Result<(), Error> {
        if o.properties.is_empty() {
            self.b().plain(Bytecode::CreateEmptyObjectLiteral);
            return Ok(());
        }
        let mut properties = Vec::new();
        let mut in_boilerplate = 0;
        for p in &o.properties {
            let ObjectPropertyKind::ObjectProperty(p) = p else { break };
            if p.computed || p.kind != PropertyKind_::Init || p.method {
                break;
            }
            let Some(name) = self.static_name(&p.key, false) else { break };
            if name == "__proto__" && !p.shorthand {
                break;
            }
            properties.push((name.into(), Self::literal_value(&p.value)));
            in_boilerplate += 1;
        }
        let shallow = properties.iter().all(|(_, v): &(Box<str>, Option<LiteralValue>)| v.is_some());
        let obj = self.temp();
        if properties.is_empty() {
            self.b().plain(Bytecode::CreateEmptyObjectLiteral);
        } else {
            let k = self.constant(Constant::Object(ObjectBoilerplate { properties }));
            let flags = CreateObjectLiteralFlags::encode(if shallow { LiteralFlags::IS_SHALLOW } else { LiteralFlags::NO_FLAGS }, shallow);
            self.b().create_object_literal(k, flags);
        }
        self.b().star(obj);
        for (i, p) in o.properties.iter().enumerate() {
            let p = match p {
                ObjectPropertyKind::ObjectProperty(p) => p,
                ObjectPropertyKind::SpreadProperty(sp) => {
                    let args = self.b().new_register_list(2);
                    self.b().mov(obj, args.first);
                    self.expr(&sp.argument)?;
                    self.b().star(args.at(1));
                    self.b().call_runtime(Runtime::InlineCopyDataProperties, args);
                    self.release(args.first);
                    continue;
                }
            };
            if i < in_boilerplate {
                // Already in the boilerplate unless its value is computed
                if Self::literal_value(&p.value).is_none() {
                    let name = self.static_name(&p.key, false).unwrap_or_default();
                    self.expr_named(&p.value, name)?;
                    self.b().define_named_own(obj, name);
                }
                continue;
            }
            let accessor = p.kind != PropertyKind_::Init;
            if accessor || p.method {
                let mkind = match p.kind {
                    PropertyKind_::Get => MethodDefinitionKind::Get,
                    PropertyKind_::Set => MethodDefinitionKind::Set,
                    PropertyKind_::Init => MethodDefinitionKind::Method,
                };
                let Expression::FunctionExpression(f) = &p.value else { return Err(self.unsupported(p.span, "method")) };
                let fname = self.method_name(&p.key, p.computed, mkind);
                let kind = match mkind {
                    MethodDefinitionKind::Get => Kind::Getter,
                    MethodDefinitionKind::Set => Kind::Setter,
                    _ => Kind::Method,
                };
                let n = self.method(fname, f, kind)?;
                if accessor {
                    // DefineAccessorPropertyUnchecked(object, name, getter, setter, attributes)
                    let args = self.b().new_register_list(5);
                    self.b().mov(obj, args.first);
                    self.property_key_into(&p.key, p.computed, args.at(1))?;
                    let (get, set) = if mkind == MethodDefinitionKind::Get { (args.at(2), args.at(3)) } else { (args.at(3), args.at(2)) };
                    self.closure(n);
                    self.b().star(get);
                    self.b().load_null();
                    self.b().star(set);
                    self.b().load_smi(0);
                    self.b().star(args.at(4));
                    self.b().call_runtime(Runtime::DefineAccessorPropertyUnchecked, args);
                    self.release(args.first);
                    continue;
                }
                self.closure(n);
            } else {
                match self.static_name(&p.key, p.computed) {
                    // `__proto__: v` sets the prototype (V8's `InternalSetPrototype`)
                    Some("__proto__") if !p.shorthand && !p.computed => {
                        let args = self.b().new_register_list(2);
                        self.b().mov(obj, args.first);
                        self.expr(&p.value)?;
                        self.b().star(args.at(1));
                        self.b().call_runtime(Runtime::InternalSetPrototype, args);
                        self.release(args.first);
                        continue;
                    }
                    Some(n) => self.expr_named(&p.value, n)?,
                    None => {}
                }
            }
            match (self.static_name(&p.key, p.computed), p.computed) {
                (Some(name), false) if !p.method => self.b().define_named_own(obj, name),
                _ => {
                    // DefineKeyedOwnPropertyInLiteral: the value waits in a register while the key is computed
                    let v = self.temp();
                    if p.method || self.static_name(&p.key, p.computed).is_some() {
                        self.b().star(v);
                    } else {
                        self.expr(&p.value)?;
                        self.b().star(v);
                    }
                    let key = self.temp();
                    self.property_key_into(&p.key, p.computed, key)?;
                    self.b().ldar(v);
                    self.b().define_keyed_own_in_literal(obj, key, 0);
                    self.release(v);
                }
            }
        }
        self.b().ldar(obj);
        self.release(obj);
        let _ = TypeOfLiteral::Other;
        Ok(())
    }
}
