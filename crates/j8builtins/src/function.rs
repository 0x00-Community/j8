use j8framework::ValueKind;
use alloc::string::String;
use alloc::vec::Vec;

use j8framework::{Agent, CodeKind, Error, Native, ObjectKind, Property, Slot, Value};

use crate::{Args, Installer};

struct Functions;

impl Functions {
    fn dynamic<'gc>(a: &mut Agent<'gc, '_>, args: &[Value<'gc>], new_target: Value<'gc>, prefix: &str) -> Result<Value<'gc>, Error> {
        let (params, body) = match args.split_last() {
            Some((&body, params)) => (params, body),
            None => (&[][..], Value::Undefined),
        };
        let mut hidden = false;
        let mut params_text = Vec::with_capacity(params.len());
        for &p in params {
            let s = a.to_string(p)?;
            hidden |= s.lone_surrogates();
            params_text.push(s.source_text());
        }
        let body = match body.kind() {
            ValueKind::Undefined => String::new(),
            b => {
                let s = a.to_string(Value::from(b))?;
                hidden |= s.lone_surrogates();
                s.source_text()
            }
        };
        let src = alloc::format!("({prefix} anonymous({}\n) {{\n{body}\n}})", params_text.join(","));
        let f = a.eval_text(&src, CodeKind::Eval, hidden)?;
        if let Some(fo) = f.as_object()
            && new_target != Value::Undefined
        {
            let fallback = fo.borrow().proto().unwrap_or(a.realm.function_proto);
            let proto = a.proto_from_ctor(new_target, fallback)?;
            fo.borrow_mut(a.mc).set_proto(Some(proto));
        }
        Ok(f)
    }

    fn function<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dynamic(a, args, a.new_target, "function")
    }

    fn generator_function<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dynamic(a, args, a.new_target, "function*")
    }

    fn async_function<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dynamic(a, args, a.new_target, "async function")
    }

    fn async_generator_function<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Self::dynamic(a, args, a.new_target, "async function*")
    }

    fn eval<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        match args.at(0).kind() {
            ValueKind::String(s) => {
                let src = s.source_text();
                a.eval_text(&src, CodeKind::Eval, s.lone_surrogates())
            }
            v => Ok(Value::from(v)),
        }
    }
}

impl<'gc> Installer<'_, 'gc> {
    pub(crate) fn functions(&mut self) {
        let r = self.realm;
        let ctor = self.constructor("Function", Functions::function, r.function_proto, 1);
        let kinds: [(&str, j8framework::NativeFn, j8framework::JsObject<'gc>, Option<j8framework::JsObject<'gc>>); 3] = [
            ("GeneratorFunction", Functions::generator_function, r.generator_function_proto, Some(r.generator_proto)),
            ("AsyncFunction", Functions::async_function, r.async_function_proto, None),
            ("AsyncGeneratorFunction", Functions::async_generator_function, r.async_generator_function_proto, Some(r.async_generator_proto)),
        ];
        for (name, f, proto, instances) in kinds {
            let c = r.named_native(self.mc, f, name, 1);
            if let ObjectKind::Native(n) = &mut c.borrow_mut(self.mc).kind {
                n.constructor = true;
            }
            let fixed = |v: Value<'gc>| Property { slot: Slot::Data(v), enumerable: false, writable: false, configurable: false };
            c.borrow_mut(self.mc).define("prototype", fixed(proto.into()));
            c.borrow_mut(self.mc).set_proto(Some(ctor));
            proto.borrow_mut(self.mc).define("constructor", Property { writable: false, ..Property::hidden(c.into()) });
            if let Some(i) = instances {
                proto.borrow_mut(self.mc).define("prototype", Property { writable: false, ..Property::hidden(i.into()) });
                i.borrow_mut(self.mc).define("constructor", Property { writable: false, ..Property::hidden(proto.into()) });
            }
        }
        r.eval.borrow_mut(self.mc).kind = ObjectKind::Native(Native::function(Functions::eval));
        r.name_function(self.mc, r.eval, "eval", Some(1));
        r.global.borrow_mut(self.mc).define("eval", Property::hidden(r.eval.into()));
    }
}
