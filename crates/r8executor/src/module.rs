//! Loading, linking and evaluation of ES modules (synchronous: dependencies are read in via `Platform::load_module`;
//! top-level await is not supported yet).
#![cfg_attr(not(feature = "compile"), allow(unused_imports, reason = "loading / linking / evaluation all need the compiler; without it only reads of import bindings remain"))]

use alloc::string::String;
use alloc::vec::Vec;

use r8framework::{Agent, Closure, Error, ErrorKind, ImportName, ModuleStatus, ObjectData, Resolution, ResolvedBinding, Value};

use crate::{Executor, Outcome, State};

impl Executor {
    /// Loads the module at `path` and its dependencies (an already-loaded module just returns its id). A compile error
    /// or a missing dependency gives `Error::Syntax`.
    #[cfg(feature = "compile")]
    pub(crate) fn load_module(&self, a: &mut Agent<'_, '_>, path: &str, text: &str) -> Result<u32, Error> {
        if let Some(i) = a.realm.modules.borrow().iter().position(|m| **m.path == *path) {
            return Ok(u32::try_from(i).unwrap_or(u32::MAX));
        }
        let (module, info) = r8virtasm::Module::compile_module(&r8framework::Source { text, file: path })?;
        let main = self.load(module);
        let mut slots = alloc::vec![Value::Undefined; usize::from(info.slots)];
        for &s in &info.lexical {
            slots[usize::from(s)] = Value::Uninit;
        }
        let env = r8framework::Env { slots, parent: None }.alloc(a.mc);
        let index = u32::try_from(a.realm.modules.borrow().len()).unwrap_or(u32::MAX);
        let info = alloc::rc::Rc::new(info);
        let record = r8framework::ModuleRecord { path: a.string(path), env, info: alloc::rc::Rc::clone(&info), main, status: ModuleStatus::Linked, error: None, requested: Vec::new(), imports: Vec::new(), namespace: None, meta: None };
        a.realm.modules.borrow_mut(a.mc).push(record);
        self.module_of.borrow_mut().insert(main.0 - r8virtasm::Module::MAIN, index);
        let mut requested = Vec::with_capacity(info.requests.len());
        for spec in &info.requests {
            let Some((p, t)) = a.platform.load_module(path, spec) else {
                return Err(Error::Syntax(alloc::format!("{path}: Cannot find module '{spec}'")));
            };
            requested.push(self.load_module(a, &p, &t)?);
        }
        a.realm.modules.borrow_mut(a.mc)[index as usize].requested = requested;
        Ok(index)
    }

    /// Linking: resolves each module's imports and re-exports (unresolvable / ambiguous gives `Error::Syntax`).
    #[cfg(feature = "compile")]
    pub(crate) fn link(a: &mut Agent<'_, '_>, m: u32, seen: &mut Vec<u32>) -> Result<(), Error> {
        if seen.contains(&m) {
            return Ok(());
        }
        seen.push(m);
        let (info, requested, path, done) = {
            let mods = a.realm.modules.borrow();
            let r = &mods[m as usize];
            (alloc::rc::Rc::clone(&r.info), r.requested.clone(), String::from(&**r.path), r.imports.len() == r.info.imports.len() && !r.info.imports.is_empty())
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
        a.realm.modules.borrow_mut(a.mc)[m as usize].imports = imports;
        Ok(())
    }

    /// Evaluation: dependencies are evaluated first (cycles skipped), each module runs only once; re-evaluating a
    /// module that threw throws the same error again.
    #[cfg(feature = "compile")]
    pub(crate) fn evaluate(&self, a: &mut Agent<'_, '_>, m: u32, depth: usize) -> Result<(), Error> {
        let (status, error, requested, main, env) = {
            let mods = a.realm.modules.borrow();
            let r = &mods[m as usize];
            (r.status, r.error, r.requested.clone(), r.main, r.env)
        };
        match status {
            ModuleStatus::Evaluated => return error.map_or(Ok(()), |e| Err(a.throw_value(e))),
            ModuleStatus::Evaluating => return Ok(()),
            ModuleStatus::Linked => {}
        }
        a.realm.modules.borrow_mut(a.mc)[m as usize].status = ModuleStatus::Evaluating;
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
            let f = self.enter(a, &mut regs, Closure::new(main, Some(env)), Value::Undefined, Value::Undefined, &[]);
            r = self.execute(a, State { regs, frames: Vec::new(), f, acc: Value::Undefined }, None).map(Outcome::returned).map(drop);
            self.depth.set(saved);
        }
        let error = match &r {
            Err(Error::Exception(g)) => Some(a.local(g)),
            _ => None,
        };
        let mut mods = a.realm.modules.borrow_mut(a.mc);
        mods[m as usize].status = ModuleStatus::Evaluated;
        mods[m as usize].error = error;
        r
    }

    /// The module a function belongs to (`None` for functions in scripts).
    pub(crate) fn module_index(&self, base: u32) -> Option<u32> {
        self.module_of.borrow().get(&base).copied()
    }

    /// `LdaImport`: the value of this module's `index`-th import binding.
    pub(crate) fn import_value<'gc>(&self, a: &mut Agent<'gc, '_>, base: u32, index: u16) -> Result<Value<'gc>, Error> {
        let Some(m) = self.module_index(base) else { return Err(a.throw(ErrorKind::Reference, "import binding outside a module")) };
        let b = a.realm.modules.borrow()[m as usize].imports[usize::from(index)];
        a.binding_value(b, "import")
    }

    /// `import.meta`: one per module (no prototype).
    pub(crate) fn import_meta<'gc>(&self, a: &mut Agent<'gc, '_>, base: u32) -> Result<Value<'gc>, Error> {
        let Some(m) = self.module_index(base) else { return Err(a.throw(ErrorKind::Syntax, "Cannot use 'import.meta' outside a module")) };
        if let Some(meta) = a.realm.modules.borrow()[m as usize].meta {
            return Ok(meta.into());
        }
        let meta = ObjectData::default().alloc(a.mc);
        let url = a.realm.modules.borrow()[m as usize].path;
        meta.borrow_mut(a.mc).set("url", url.into());
        a.realm.modules.borrow_mut(a.mc)[m as usize].meta = Some(meta);
        Ok(meta.into())
    }

    /// Loads, links and evaluates a module, returning its id.
    #[cfg(feature = "compile")]
    pub(crate) fn run_module(&self, a: &mut Agent<'_, '_>, path: &str, text: &str, depth: usize) -> Result<u32, Error> {
        let m = self.load_module(a, path, text)?;
        Self::link(a, m, &mut Vec::new())?;
        self.evaluate(a, m, depth)?;
        Ok(m)
    }

    /// `import(specifier)`: returns a Promise; loading / linking / evaluation all happen synchronously here, and the
    /// Promise is fulfilled with the namespace object.
    pub(crate) fn dynamic_import<'gc>(&self, a: &mut Agent<'gc, '_>, referrer: &str, specifier: Value<'gc>, depth: usize) -> Result<Value<'gc>, Error> {
        let promise = a.new_promise();
        let r = (|| -> Result<Value<'gc>, Error> {
            let spec = a.to_string(specifier)?;
            #[cfg(feature = "compile")]
            {
                let Some((path, text)) = a.platform.load_module(referrer, &spec) else {
                    return Err(a.throw(ErrorKind::Type, &alloc::format!("Cannot find module '{}'", &**spec)));
                };
                let m = self.run_module(a, &path, &text, depth)?;
                Ok(a.module_namespace(m).into())
            }
            #[cfg(not(feature = "compile"))]
            {
                let _ = (referrer, depth);
                Err(a.throw(ErrorKind::Type, &alloc::format!("Cannot load module '{}'", &**spec)))
            }
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
