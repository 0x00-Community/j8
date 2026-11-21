use alloc::string::String;
use j8compiler::Compile;
use alloc::vec::Vec;

use j8framework::{Agent, Closure, Error, ErrorKind, ImportName, ModuleStatus, ObjectData, Resolution, ResolvedBinding, Value};

use crate::{Executor, Outcome};

impl Executor {
    pub fn load_module(&self, a: &mut Agent<'_, '_>, path: &str, text: &str) -> Result<u32, Error> {
        if let Some(i) = a.modules().iter().position(|m| **m.path == *path) {
            return Ok(u32::try_from(i).unwrap_or(u32::MAX));
        }
        let src = j8framework::Source { text, file: path };
        let (module, info) = match &self.cache {
            Some(cache) => cache.module(&src)?,
            None => <j8bytecode::Module as Compile>::compile_module(&src).map(|(m, info)| (m, alloc::sync::Arc::new(info)))?,
        };
        let main = self.load(module);
        let mut slots = alloc::vec![Value::Undefined; usize::from(info.slots)];
        for &s in &info.lexical {
            slots[usize::from(s)] = Value::Uninit;
        }
        let env = j8framework::Env::new(slots, None).alloc(a.mc);
        let record = j8framework::ModuleRecord { path: a.string(path), env, info: alloc::sync::Arc::clone(&info), main, status: ModuleStatus::Linked, error: None, requested: Vec::new(), imports: Vec::new(), namespace: None, meta: None };
        let index = a.push_module(&record);
        self.module_of.borrow_mut().insert(main.0 - j8bytecode::Module::MAIN, index);
        let mut requested = Vec::with_capacity(info.requests.len());
        for spec in &info.requests {
            let Some((p, t)) = a.platform.load_module(path, spec) else {
                return Err(Error::Syntax(alloc::format!("{path}: Cannot find module '{spec}'")));
            };
            requested.push(self.load_module(a, &p, &t)?);
        }
        a.update_module(index, |r| r.requested.clone_from(&requested));
        Ok(index)
    }

    pub fn link(a: &mut Agent<'_, '_>, m: u32, seen: &mut Vec<u32>) -> Result<(), Error> {
        if seen.contains(&m) {
            return Ok(());
        }
        seen.push(m);
        let (info, requested, path, done) = {
            let r = &a.modules()[m as usize];
            (alloc::sync::Arc::clone(&r.info), r.requested.clone(), String::from(&**r.path), r.imports.len() == r.info.imports.len() && !r.info.imports.is_empty())
        };
        for &r in &requested {
            Self::link(a, r, seen)?;
        }
        if done {
            return Ok(());
        }
        let mut imports = Vec::with_capacity(info.imports.len());
        for e in &info.imports {
            let target = requested[e.request];
            let b = match &e.name {
                ImportName::Namespace => ResolvedBinding::Namespace(target),
                ImportName::Named(n) => match a.resolve_export(target, n, &mut Vec::new()) {
                    Resolution::Found(b) => b,
                    Resolution::NotFound => return Err(Error::Syntax(alloc::format!("{path}: The requested module '{}' does not provide an export named '{n}'", info.requests[e.request]))),
                    Resolution::Ambiguous => return Err(Error::Syntax(alloc::format!("{path}: The requested module '{}' contains conflicting star exports for name '{n}'", info.requests[e.request]))),
                },
            };
            imports.push(b);
        }
        for (name, _, _) in &info.indirect_exports {
            if !matches!(a.resolve_export(m, name, &mut Vec::new()), Resolution::Found(_)) {
                return Err(Error::Syntax(alloc::format!("{path}: cannot resolve export '{name}'")));
            }
        }
        a.update_module(m, |r| r.imports.clone_from(&imports));
        Ok(())
    }

    pub fn evaluate(&self, a: &mut Agent<'_, '_>, m: u32, depth: usize) -> Result<(), Error> {
        let (status, error, requested, main, env) = {
            let r = &a.modules()[m as usize];
            (r.status, r.error, r.requested.clone(), r.main, r.env)
        };
        match status {
            ModuleStatus::Evaluated => return error.map_or(Ok(()), |e| Err(a.throw_value(e))),
            ModuleStatus::Evaluating => return Ok(()),
            ModuleStatus::Linked => {}
        }
        a.update_module(m, |r| r.status = ModuleStatus::Evaluating);
        let mut r = Ok(());
        for &dep in &requested {
            r = self.evaluate(a, dep, depth);
            if r.is_err() {
                break;
            }
        }
        if r.is_ok() {
            let saved = self.depth.replace(depth + 1);
            let mut regs = Vec::new();
            let f = self.open(a, &mut regs, Closure::new(main, Some(env)), Value::Undefined, Value::Undefined, &[]);
            r = self.execute(a, self.state(regs, f), &mut None).map(Outcome::returned).map(drop);
            self.depth.set(saved);
        }
        let error = match &r {
            Err(Error::Exception(g)) => Some(a.local(g)),
            _ => None,
        };
        a.update_module(m, |r| {
            r.status = ModuleStatus::Evaluated;
            r.error = error;
        });
        r
    }

    pub fn module_index(&self, base: u32) -> Option<u32> {
        self.module_of.borrow().get(&base).copied()
    }

    pub fn import_value<'gc>(&self, a: &mut Agent<'gc, '_>, base: u32, index: u16) -> Result<Value<'gc>, Error> {
        let Some(m) = self.module_index(base) else { return Err(a.throw(ErrorKind::Reference, "import binding outside a module")) };
        let b = a.modules()[m as usize].imports[usize::from(index)];
        a.binding_value(b, "import")
    }

    pub fn import_meta<'gc>(&self, a: &mut Agent<'gc, '_>, base: u32) -> Result<Value<'gc>, Error> {
        let Some(m) = self.module_index(base) else { return Err(a.throw(ErrorKind::Syntax, "Cannot use 'import.meta' outside a module")) };
        if let Some(meta) = a.modules()[m as usize].meta {
            return Ok(meta.into());
        }
        let meta = ObjectData::default().alloc(a.mc);
        let url = a.modules()[m as usize].path;
        meta.borrow_mut(a.mc).set("url", url.into());
        a.update_module(m, |r| r.meta = Some(meta));
        Ok(meta.into())
    }

    pub fn run_module(&self, a: &mut Agent<'_, '_>, path: &str, text: &str, depth: usize) -> Result<u32, Error> {
        let m = self.load_module(a, path, text)?;
        Self::link(a, m, &mut Vec::new())?;
        self.evaluate(a, m, depth)?;
        Ok(m)
    }

    pub fn dynamic_import<'gc>(&self, a: &mut Agent<'gc, '_>, referrer: &str, specifier: Value<'gc>, depth: usize) -> Result<Value<'gc>, Error> {
        let promise = a.new_promise();
        let r = (|| -> Result<Value<'gc>, Error> {
            let spec = a.to_string(specifier)?;
            let Some((path, text)) = a.platform.load_module(referrer, &spec) else {
                return Err(a.throw(ErrorKind::Type, &alloc::format!("Cannot find module '{}'", &**spec)));
            };
            let m = self.run_module(a, &path, &text, depth)?;
            Ok(a.module_namespace(m).into())
        })();
        match r {
            Ok(ns) => a.resolve_promise(promise, ns)?,
            Err(Error::Exception(g)) => {
                let reason = a.local(&g);
                a.reject_promise(promise, reason);
            }
            Err(Error::Syntax(msg)) => {
                let e = a.error(ErrorKind::Syntax, &msg);
                a.reject_promise(promise, e.into());
            }
            Err(e) => return Err(e),
        }
        Ok(promise.into())
    }
}
