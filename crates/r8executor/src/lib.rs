//! r8executor：r8bytecode 字节码执行器（`no_std`）。
#![no_std]
extern crate alloc;

use alloc::vec;

use r8bytecode::{Constant, Insn, Module};
use r8frame::{Error, FuncId, Scope, Value};

/// 字节码执行器。
#[derive(Debug, Default)]
pub struct Executor;

impl Executor {
    /// 执行 `module` 的函数 `f`。
    pub fn call<'gc>(&mut self, scope: &mut Scope<'gc, '_>, module: &Module, f: FuncId) -> Result<Value<'gc>, Error> {
        scope.profiler.enter(f);
        let func = module.function(f);
        let mut regs = vec![Value::Undefined; usize::from(func.regs)];
        let mut acc = Value::Undefined;
        for insn in &func.insns {
            match *insn {
                Insn::LdaConst(k) => {
                    acc = match &func.consts[k as usize] {
                        Constant::Number(x) => (*x).into(),
                        Constant::String(s) => scope.string(s).into(),
                    }
                }
                Insn::LdaUndefined => acc = Value::Undefined,
                Insn::LdaNull => acc = Value::Null,
                Insn::LdaBool(b) => acc = b.into(),
                Insn::Star(r) => regs[usize::from(r.0)] = acc,
                Insn::Ldar(r) => acc = regs[usize::from(r.0)],
                Insn::Binary(op, r) => acc = scope.binary(op, regs[usize::from(r.0)], acc),
                Insn::Negate => acc = (-scope.to_number(acc)).into(),
                Insn::Return => return Ok(acc),
            }
        }
        Ok(acc)
    }
}

#[cfg(feature = "compile")]
impl r8frame::Engine for Executor {
    fn eval<'gc>(&mut self, scope: &mut Scope<'gc, '_>, _cx: r8frame::Context<'gc>, src: &r8frame::Source<'_>) -> Result<Value<'gc>, Error> {
        let module = Module::try_from(src)?;
        self.call(scope, &module, Module::MAIN)
    }
}

#[cfg(test)]
mod tests {
    use r8frame::{CreateParams, Isolate, Source, Value};
    use r8platform::DefaultPlatform;

    use crate::Executor;

    #[test]
    fn arithmetic_and_concat() {
        let platform = DefaultPlatform::default();
        let mut isolate = Isolate::new(&platform, CreateParams::default());
        isolate.enter(|scope| {
            let cx = scope.context();
            let v = scope.run(cx, &mut Executor, &Source { text: "1 + 2 * 3", file: "t.js" }).unwrap();
            assert_eq!(v, Value::Number(7.0));
            let Value::String(s) = scope.run(cx, &mut Executor, &Source { text: "'a' + 1", file: "t.js" }).unwrap() else { panic!("a string") };
            assert_eq!(&**s, "a1");
        });
    }
}
