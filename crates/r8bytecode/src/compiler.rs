use oxc_allocator::Allocator;
use oxc_ast::ast::{Expression, Statement, UnaryOperator};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use r8::{BinaryOp, Error, Source, Tier};

use crate::{Constant, Function, Insn, Module, Reg};

/// 正在编的一个函数。
struct Emit<'s> {
    src: &'s Source<'s>,
    func: Function,
    /// 下一个空寄存器
    top: u16,
}

impl TryFrom<&Source<'_>> for Module {
    type Error = Error;

    /// 编一个脚本：最后一条表达式语句的值留在 acc 里返回。
    fn try_from(src: &Source<'_>) -> Result<Self, Error> {
        let allocator = Allocator::default();
        let ret = Parser::new(&allocator, src.text, SourceType::cjs()).parse();
        if let Some(d) = ret.diagnostics.first() {
            return Err(Error::Syntax(format!("{}: {d}", src.file)));
        }
        let mut emit = Emit { src, func: Function::default(), top: 0 };
        emit.func.insns.push(Insn::LdaUndefined);
        ret.program.body.iter().try_for_each(|s| emit.statement(s))?;
        emit.func.insns.push(Insn::Return);
        Ok(Module { functions: vec![emit.func] })
    }
}

impl Emit<'_> {
    fn unsupported(&self, offset: u32, what: &str) -> Error {
        // 字节码层的不支持算在执行它的 mercedes 上
        self.src.unsupported(Tier::Mercedes, offset, what)
    }

    fn constant(&mut self, c: Constant) -> Insn {
        let k = self.func.consts.iter().position(|x| *x == c).unwrap_or_else(|| {
            self.func.consts.push(c);
            self.func.consts.len() - 1
        });
        Insn::LdaConst(u32::try_from(k).expect("fewer than 2^32 constants"))
    }

    fn statement(&mut self, s: &Statement<'_>) -> Result<(), Error> {
        match s {
            Statement::ExpressionStatement(e) => self.expression(&e.expression),
            Statement::EmptyStatement(_) => Ok(()),
            s => Err(self.unsupported(s.span().start, "statement")),
        }
    }

    /// 求值 `e`，结果留在 acc。
    fn expression(&mut self, e: &Expression<'_>) -> Result<(), Error> {
        let insn = match e {
            Expression::NumericLiteral(n) => self.constant(Constant::Number(n.value)),
            Expression::BooleanLiteral(b) => Insn::LdaBool(b.value),
            Expression::NullLiteral(_) => Insn::LdaNull,
            Expression::StringLiteral(s) => {
                let text = s.value.as_str().ok_or_else(|| self.unsupported(s.span.start, "lone surrogate"))?;
                self.constant(Constant::String(text.into()))
            }
            Expression::Identifier(i) if i.name == "undefined" => Insn::LdaUndefined,
            Expression::ParenthesizedExpression(p) => return self.expression(&p.expression),
            Expression::UnaryExpression(u) if u.operator == UnaryOperator::UnaryNegation => {
                self.expression(&u.argument)?;
                Insn::Negate
            }
            Expression::BinaryExpression(b) => {
                let op = BinaryOp::try_from(b.operator).map_err(|op| self.unsupported(b.span.start, op.as_str()))?;
                self.expression(&b.left)?;
                let r = Reg(self.top);
                self.top += 1;
                self.func.regs = self.func.regs.max(self.top);
                self.func.insns.push(Insn::Star(r));
                self.expression(&b.right)?;
                self.top -= 1;
                Insn::Binary(op, r)
            }
            e => return Err(self.unsupported(e.span().start, "expression")),
        };
        self.func.insns.push(insn);
        Ok(())
    }
}
