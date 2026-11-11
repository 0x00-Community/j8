use alloc::string::String;
use alloc::vec::Vec;

use r8frame::{Agent, CodeKind, Error, Native, ObjectKind, Property, Slot, Value};

use crate::{Args, Installer};

/// `Function` 一族构造器与全局 `eval`（动态代码经 [`Agent::eval_code`]）。
struct Functions;

impl Functions {
    /// CreateDynamicFunction：形参与函数体拼成 `(前缀 anonymous(形参\n) {\n函数体\n})` 编译求值。
    fn dynamic<'gc>(a: &mut Agent<'gc, '_>, args: &[Value<'gc>], prefix: &str) -> Result<Value<'gc>, Error> {
        let (params, body) = match args.split_last() {
            Some((&body, params)) => (params, body),
            None => (&[][..], Value::Undefined),
        };
        let params = params.iter().map(|&p| a.to_string(p).map(|s| String::from(&**s))).collect::<Result<Vec<_>, _>>()?;
        let body = match body {
            Value::Undefined => String::new(),
            b => String::from(&**a.to_string(b)?),
        };
        let src = alloc::format!("({prefix} anonymous({}\n) {{\n{body}\n}})", params.join(","));
        a.eval_code(&src, CodeKind::Eval)
    }

    fn function<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dynamic(a, args, "function")
    }

    fn generator_function<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dynamic(a, args, "function*")
    }

    fn async_function<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dynamic(a, args, "async function")
    }

    fn async_generator_function<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dynamic(a, args, "async function*")
    }

    /// 全局 `eval`（间接）：不是字符串原样返回，是就当全局代码跑。
    fn eval<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        match args.at(0) {
            Value::String(s) => {
                let src = String::from(&**s);
                a.eval_code(&src, CodeKind::Eval)
            }
            v => Ok(v),
        }
    }
}

impl<'gc> Installer<'_, 'gc> {
    pub(crate) fn functions(&mut self) {
        let r = self.realm;
        let ctor = self.constructor("Function", Functions::function, r.function_proto);
        self.length(ctor, 1);
        // 生成器 / async 函数构造器不是全局的：经各自原型的 `constructor` 取到
        let kinds: [(&str, r8frame::NativeFn, r8frame::JsObject<'gc>, Option<r8frame::JsObject<'gc>>); 3] = [
            ("GeneratorFunction", Functions::generator_function, r.generator_function_proto, Some(r.generator_proto)),
            ("AsyncFunction", Functions::async_function, r.async_function_proto, None),
            ("AsyncGeneratorFunction", Functions::async_generator_function, r.async_generator_function_proto, Some(r.async_generator_proto)),
        ];
        for (name, f, proto, instances) in kinds {
            let c = r.named_native(self.mc, f, name);
            self.length(c, 1);
            let fixed = |v: Value<'gc>| Property { slot: Slot::Data(v), enumerable: false, writable: false, configurable: false };
            c.borrow_mut(self.mc).define(self.mc, "prototype", fixed(proto.into()));
            c.borrow_mut(self.mc).proto = Some(ctor);
            proto.borrow_mut(self.mc).define(self.mc, "constructor", Property { writable: false, ..Property::hidden(c.into()) });
            if let Some(i) = instances {
                proto.borrow_mut(self.mc).define(self.mc, "prototype", Property { writable: false, ..Property::hidden(i.into()) });
                i.borrow_mut(self.mc).define(self.mc, "constructor", Property { writable: false, ..Property::hidden(proto.into()) });
            }
        }
        // %eval% 是 Realm 里预留的那个对象（执行器靠它认出直接 eval）
        r.eval.borrow_mut(self.mc).kind = ObjectKind::Native(Native(Functions::eval));
        r.name_function(self.mc, r.eval, "eval", Some(1));
        r.global.borrow_mut(self.mc).define(self.mc, "eval", Property::hidden(r.eval.into()));
    }
}
