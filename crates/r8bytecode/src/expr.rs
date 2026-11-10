//! JS ⇒ 字节码：表达式。结果都留在 acc。
use oxc_ast::ast::{
    ArrowFunctionBody, ArrowFunctionExpression, AssignmentExpression, AssignmentOperator, BinaryExpression, BinaryOperator, CallExpression, Expression, LogicalOperator, MemberExpression, ObjectExpression, ObjectPropertyKind, PropertyKey, PropertyKind, SimpleAssignmentTarget, TemplateElement, UnaryOperator, UpdateExpression,
    UpdateOperator,
};
use oxc_span::{GetSpan, Span};
use r8frame::{BinaryOp, CompareOp, Error, UnaryOp};

use crate::compiler::{Body, Compiler, Place};
use crate::{Const, Constant, Insn, Reg};

/// 二元运算符 ⇒ 字节码运算。
enum Operator {
    Binary(BinaryOp),
    Compare(CompareOp),
}

/// 赋值运算符的三类。
enum Assign {
    Plain,
    Binary(BinaryOp),
    /// `||=` / `&&=` / `??=`：左边已经满足就不求右边
    Logical(fn(crate::Label) -> Insn),
}

/// 赋值目标：名字，或已经求好对象（与键）的属性。
enum Target<'a> {
    Name(&'a str),
    Named { obj: Reg, name: Const },
    Keyed { obj: Reg, key: Reg },
}

impl<'a> Compiler<'a, '_> {
    fn operator(&self, op: BinaryOperator, span: Span) -> Result<Operator, Error> {
        Ok(match op {
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
            BinaryOperator::In | BinaryOperator::Instanceof => return Err(self.unsupported(span, op.as_str())),
        })
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

    fn string_const(&mut self, s: Option<&str>, span: Span) -> Result<Const, Error> {
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
            Expression::Identifier(id) => self.load(id.name.as_str()),
            Expression::ThisExpression(_) => self.load(crate::scope::THIS),
            Expression::ParenthesizedExpression(p) => self.expr(&p.expression)?,
            Expression::FunctionExpression(f) => {
                let n = self.plain_function(f, true)?;
                self.emit(Insn::CreateClosure(n));
            }
            Expression::ArrowFunctionExpression(a) => self.arrow(a)?,
            Expression::UnaryExpression(u) => {
                let op = match u.operator {
                    UnaryOperator::UnaryNegation => UnaryOp::Neg,
                    UnaryOperator::UnaryPlus => UnaryOp::Plus,
                    UnaryOperator::LogicalNot => UnaryOp::Not,
                    UnaryOperator::BitwiseNot => UnaryOp::BitNot,
                    UnaryOperator::Typeof => UnaryOp::Typeof,
                    UnaryOperator::Void => UnaryOp::Void,
                    UnaryOperator::Delete => return Err(self.unsupported(u.span, "delete")),
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
            Expression::StaticMemberExpression(m) => {
                if m.optional {
                    return Err(self.unsupported(m.span, "optional chaining"));
                }
                self.expr(&m.object)?;
                let obj = self.temp();
                self.emit(Insn::Star(obj));
                let name = self.name(m.property.name.as_str());
                self.emit(Insn::GetNamed { obj, name });
                self.release(obj);
            }
            Expression::ComputedMemberExpression(m) => {
                if m.optional {
                    return Err(self.unsupported(m.span, "optional chaining"));
                }
                self.expr(&m.object)?;
                let obj = self.temp();
                self.emit(Insn::Star(obj));
                self.expr(&m.expression)?;
                self.emit(Insn::GetKeyed { obj });
                self.release(obj);
            }
            Expression::ObjectExpression(o) => self.object(o)?,
            Expression::ArrayExpression(a) => {
                let count = u16::try_from(a.elements.len()).map_err(|_| self.unsupported(a.span, "more than 65535 elements"))?;
                let first = self.temps(count);
                for (i, el) in (0..).zip(&a.elements) {
                    let e = el.as_expression().ok_or_else(|| self.unsupported(el.span(), "spread / holes in array literals"))?;
                    self.expr(e)?;
                    self.emit(Insn::Star(Reg(first.0 + i)));
                }
                self.emit(Insn::CreateArray { first, count });
                self.release(first);
            }
            e => return Err(self.unsupported(e.span(), "expression")),
        }
        Ok(())
    }

    fn arrow(&mut self, a: &'a ArrowFunctionExpression<'a>) -> Result<(), Error> {
        if a.r#async {
            return Err(self.unsupported(a.span, "async arrow functions"));
        }
        let body = match &a.body {
            ArrowFunctionBody::FunctionBody(b) => Body::Statements(&b.statements),
            e => Body::Expression(e.as_expression().expect("an arrow's body is a block or an expression")),
        };
        let n = self.function("", None, None, &a.params, body)?;
        self.emit(Insn::CreateClosure(n));
        Ok(())
    }

    fn binary(&mut self, b: &'a BinaryExpression<'a>) -> Result<(), Error> {
        let op = self.operator(b.operator, b.span)?;
        self.expr(&b.left)?;
        let l = self.temp();
        self.emit(Insn::Star(l));
        self.expr(&b.right)?;
        self.emit(match op {
            Operator::Binary(op) => Insn::Binary(op, l),
            Operator::Compare(op) => Insn::Compare(op, l),
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
        }
    }

    fn target_store(&mut self, t: &Target<'a>) {
        match *t {
            Target::Name(n) => self.store(n, false),
            Target::Named { obj, name } => _ = self.emit(Insn::SetNamed { obj, name }),
            Target::Keyed { obj, key } => _ = self.emit(Insn::SetKeyed { obj, key }),
        }
    }

    fn target_release(&mut self, t: &Target<'a>) {
        match *t {
            Target::Name(_) => {}
            Target::Named { obj, .. } | Target::Keyed { obj, .. } => self.release(obj),
        }
    }

    fn assignment(&mut self, a: &'a AssignmentExpression<'a>) -> Result<(), Error> {
        let t = a.left.as_simple_assignment_target().ok_or_else(|| self.unsupported(a.left.span(), "destructuring assignment"))?;
        let t = self.target(t)?;
        match Self::assign_kind(a.operator) {
            Assign::Plain => {
                self.expr(&a.right)?;
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
                self.expr(&a.right)?;
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
        self.emit(Insn::ToNumber);
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
        if c.optional {
            return Err(self.unsupported(c.span, "optional calls"));
        }
        let argc = u16::try_from(c.arguments.len()).map_err(|_| self.unsupported(c.span, "more than 65535 arguments"))?;
        let callee = self.temps(2 + argc);
        let this = Reg(callee.0 + 1);
        match &c.callee {
            Expression::StaticMemberExpression(m) if !m.optional => {
                self.expr(&m.object)?;
                self.emit(Insn::Star(this));
                let name = self.name(m.property.name.as_str());
                self.emit(Insn::GetNamed { obj: this, name });
            }
            Expression::ComputedMemberExpression(m) if !m.optional => {
                self.expr(&m.object)?;
                self.emit(Insn::Star(this));
                self.expr(&m.expression)?;
                self.emit(Insn::GetKeyed { obj: this });
            }
            f => {
                self.emit(Insn::LdaUndefined);
                self.emit(Insn::Star(this));
                self.expr(f)?;
            }
        }
        self.emit(Insn::Star(callee));
        let first = Reg(callee.0 + 2);
        for (i, a) in (0..).zip(&c.arguments) {
            let e = a.as_expression().ok_or_else(|| self.unsupported(a.span(), "spread arguments"))?;
            self.expr(e)?;
            self.emit(Insn::Star(Reg(first.0 + i)));
        }
        self.emit(Insn::Call { callee, this, args: first, argc });
        self.release(callee);
        Ok(())
    }

    fn object(&mut self, o: &'a ObjectExpression<'a>) -> Result<(), Error> {
        self.emit(Insn::CreateObject);
        let obj = self.temp();
        self.emit(Insn::Star(obj));
        for p in &o.properties {
            let ObjectPropertyKind::ObjectProperty(p) = p else {
                return Err(self.unsupported(p.span(), "object spread"));
            };
            if p.kind != PropertyKind::Init {
                return Err(self.unsupported(p.span, "getters / setters"));
            }
            let name = match &p.key {
                PropertyKey::StaticIdentifier(id) => Some(id.name.as_str()),
                PropertyKey::StringLiteral(s) if !p.computed => Some(s.value.as_str().ok_or_else(|| self.unsupported(s.span, "lone surrogates"))?),
                PropertyKey::PrivateIdentifier(id) => return Err(self.unsupported(id.span, "private names")),
                _ => None,
            };
            match (name, &p.key) {
                (Some(n), _) => {
                    self.expr(&p.value)?;
                    let name = self.name(n);
                    self.emit(Insn::SetNamed { obj, name });
                }
                (None, PropertyKey::NumericLiteral(n)) if !p.computed => {
                    let name = self.name(ryu_js::Buffer::new().format(n.value));
                    self.expr(&p.value)?;
                    self.emit(Insn::SetNamed { obj, name });
                }
                (None, k) => {
                    let k = k.as_expression().ok_or_else(|| self.unsupported(p.span, "property key"))?;
                    self.expr(k)?;
                    let key = self.temp();
                    self.emit(Insn::Star(key));
                    self.expr(&p.value)?;
                    self.emit(Insn::SetKeyed { obj, key });
                    self.release(key);
                }
            }
        }
        self.emit(Insn::Ldar(obj));
        self.release(obj);
        Ok(())
    }
}
