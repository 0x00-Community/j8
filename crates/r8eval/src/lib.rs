//! r8eval：直接在 oxc AST 上求值的解释器（最低执行层，无需编译）。
use oxc_allocator::Allocator;
use oxc_ast::ast::{Expression, Statement, UnaryOperator};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use r8::{BinaryOp, Context, Engine, Error, Source, Tier, Value};

/// AST 解释器。
#[derive(Debug, Default)]
pub struct Interp;

/// 一次求值：借着环境与源码。
struct Walk<'c, 'i, 'p, 's> {
    cx: &'c mut Context<'i, 'p>,
    src: &'s Source<'s>,
}

impl Engine for Interp {
    fn tier(&self) -> Tier {
        Tier::Eval
    }

    fn eval(&mut self, cx: &mut Context<'_, '_>, src: &Source<'_>) -> Result<Value, Error> {
        let allocator = Allocator::default();
        let ret = Parser::new(&allocator, src.text, SourceType::cjs()).parse();
        if let Some(d) = ret.diagnostics.first() {
            return Err(Error::Syntax(format!("{}: {d}", src.file)));
        }
        let mut walk = Walk { cx, src };
        ret.program.body.iter().try_fold(Value::Undefined, |_, s| walk.statement(s))
    }
}

impl Walk<'_, '_, '_, '_> {
    fn statement(&mut self, s: &Statement<'_>) -> Result<Value, Error> {
        match s {
            Statement::ExpressionStatement(e) => self.expression(&e.expression),
            Statement::EmptyStatement(_) => Ok(Value::Undefined),
            s => Err(self.src.unsupported(Tier::Eval, s.span().start, "statement")),
        }
    }

    fn expression(&mut self, e: &Expression<'_>) -> Result<Value, Error> {
        let heap = self.cx.isolate().heap_mut();
        Ok(match e {
            Expression::NumericLiteral(n) => n.value.into(),
            Expression::BooleanLiteral(b) => b.value.into(),
            Expression::NullLiteral(_) => Value::Null,
            Expression::StringLiteral(s) => heap.intern(s.value.as_str().ok_or_else(|| self.src.unsupported(Tier::Eval, s.span.start, "lone surrogate"))?).into(),
            Expression::Identifier(i) if i.name == "undefined" => Value::Undefined,
            Expression::ParenthesizedExpression(p) => self.expression(&p.expression)?,
            Expression::UnaryExpression(u) if u.operator == UnaryOperator::UnaryNegation => {
                let x = self.expression(&u.argument)?;
                let heap = self.cx.isolate().heap_mut();
                (-heap.number(x)).into()
            }
            Expression::BinaryExpression(b) => {
                let op = BinaryOp::try_from(b.operator).map_err(|op| self.src.unsupported(Tier::Eval, b.span.start, op.as_str()))?;
                let l = self.expression(&b.left)?;
                let r = self.expression(&b.right)?;
                self.cx.isolate().heap_mut().binary(op, l, r)
            }
            e => return Err(self.src.unsupported(Tier::Eval, e.span().start, "expression")),
        })
    }
}
