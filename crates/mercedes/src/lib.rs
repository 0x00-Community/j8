//! mercedes：r8bytecode 字节码执行器。
use r8::{Context, Engine, Error, FuncId, Source, Tier, Value};
use r8bytecode::{Constant, Insn, Module};

/// 字节码解释器。
#[derive(Debug, Default)]
pub struct Mercedes;

impl Mercedes {
    /// 在 `cx` 里执行 `module` 的函数 `f`。
    pub fn call(&mut self, cx: &mut Context<'_, '_>, module: &Module, f: FuncId) -> Result<Value, Error> {
        let isolate = cx.isolate();
        isolate.profiler_mut().enter(f);
        let func = module.function(f);
        let mut regs = vec![Value::Undefined; usize::from(func.regs)];
        let mut acc = Value::Undefined;
        let heap = isolate.heap_mut();
        for insn in &func.insns {
            match *insn {
                Insn::LdaConst(k) => {
                    acc = match &func.consts[k as usize] {
                        Constant::Number(x) => (*x).into(),
                        Constant::String(s) => heap.intern(s).into(),
                    }
                }
                Insn::LdaUndefined => acc = Value::Undefined,
                Insn::LdaNull => acc = Value::Null,
                Insn::LdaBool(b) => acc = b.into(),
                Insn::Star(r) => regs[usize::from(r.0)] = acc,
                Insn::Ldar(r) => acc = regs[usize::from(r.0)],
                Insn::Binary(op, r) => acc = heap.binary(op, regs[usize::from(r.0)], acc),
                Insn::Negate => acc = (-heap.number(acc)).into(),
                Insn::Return => return Ok(acc),
            }
        }
        Ok(acc)
    }
}

impl Engine for Mercedes {
    fn tier(&self) -> Tier {
        Tier::Mercedes
    }

    fn eval(&mut self, cx: &mut Context<'_, '_>, src: &Source<'_>) -> Result<Value, Error> {
        let module = Module::try_from(src)?;
        self.call(cx, &module, Module::MAIN)
    }
}

#[cfg(test)]
mod tests {
    use r8::{CreateParams, DefaultPlatform, Isolate, Source, Value};

    use crate::Mercedes;

    #[test]
    fn arithmetic_and_concat() {
        let platform = DefaultPlatform::default();
        let mut isolate = Isolate::new(&platform, CreateParams::default());
        let mut cx = isolate.context();
        let v = cx.run(&mut Mercedes, &Source { text: "1 + 2 * 3", file: "t.js" }).unwrap();
        assert_eq!(v, Value::Number(7.0));
        let Value::String(s) = cx.run(&mut Mercedes, &Source { text: "'a' + 1", file: "t.js" }).unwrap() else { panic!("a string") };
        assert_eq!(cx.isolate().heap().str(s), "a1");
    }
}
