//! ES modules: static information produced by the compiler, runtime module records, export resolution, and the property
//! behavior of namespace objects (spec 10.4.6).
use crate::ValueKind;
use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::{Collect, Gc};

use crate::{Agent, Error, ErrorKind, FuncId, JsEnv, JsObject, JsString, Key, ObjectKind, Property, PropertyKey, Slot, Value};

/// What is imported: a name, or the whole namespace (`import * as ns` / `export * as ns from`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportName {
    Named(Box<str>),
    Namespace,
}

/// One import: import `name` from the module of the `request`-th request (the index is the import binding's id).
#[derive(Clone, Debug)]
pub struct ImportEntry {
    pub request: usize,
    pub name: ImportName,
}

/// Static module information produced by the compiler.
#[derive(Clone, Debug, Default)]
pub struct ModuleInfo {
    /// The module specifiers after `from` (deduplicated, in order of appearance)
    pub requests: Vec<Box<str>>,
    pub imports: Vec<ImportEntry>,
    /// This module's own exports: export name ⇒ module environment slot
    pub local_exports: Vec<(Box<str>, u16)>,
    /// Re-exports: export name ⇒ (request, import name)
    pub indirect_exports: Vec<(Box<str>, usize, ImportName)>,
    /// `export * from`
    pub star_exports: Vec<usize>,
    /// The number of module environment slots; which slots are `let` / `const` / `class` (uninitialized at start)
    pub slots: u16,
    pub lexical: Vec<u16>,
}

/// The state of a module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Collect)]
#[collect(require_static)]
pub enum ModuleStatus {
    Linked,
    Evaluating,
    Evaluated,
}

/// A resolved binding: a slot in some module environment, or some module's namespace object.
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum ResolvedBinding<'gc> {
    Slot(JsEnv<'gc>, u16),
    Namespace(u32),
}

/// A runtime module record (kept in the Realm, numbered in load order).
#[derive(Clone, Collect)]
#[collect(no_drop)]
pub struct ModuleRecord<'gc> {
    pub path: JsString<'gc>,
    pub env: JsEnv<'gc>,
    #[collect(require_static)]
    pub info: alloc::sync::Arc<ModuleInfo>,
    pub main: FuncId,
    pub status: ModuleStatus,
    /// The exception thrown during evaluation (importing the same module again throws it again)
    pub error: Option<Value<'gc>>,
    /// The module id for each request
    pub requested: Vec<u32>,
    /// Where each import binding resolves to
    pub imports: Vec<ResolvedBinding<'gc>>,
    pub namespace: Option<JsObject<'gc>>,
    pub meta: Option<JsObject<'gc>>,
}

impl core::fmt::Debug for ModuleRecord<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ModuleRecord").field("path", &&**self.path).finish()
    }
}

/// The result of ResolveExport.
#[derive(Clone, Copy, Debug)]
pub enum Resolution<'gc> {
    Found(ResolvedBinding<'gc>),
    NotFound,
    Ambiguous,
}

impl ResolvedBinding<'_> {
    /// Whether this is the same binding.
    fn same(self, other: Self) -> bool {
        match (self, other) {
            (ResolvedBinding::Slot(e1, s1), ResolvedBinding::Slot(e2, s2)) => Gc::ptr_eq(e1, e2) && s1 == s2,
            (ResolvedBinding::Namespace(x), ResolvedBinding::Namespace(y)) => x == y,
            _ => false,
        }
    }
}

impl<'gc> Agent<'gc, '_> {
    /// The loaded modules (an immutable snapshot; the index is the module id)
    pub fn modules(&self) -> &'gc [ModuleRecord<'gc>] {
        self.realm.modules.get()
    }

    /// Changes module `m` (copy-on-write: `f` may run more than once, so it should only assign fields)
    pub fn update_module(&self, m: u32, mut f: impl FnMut(&mut ModuleRecord<'gc>)) {
        self.realm.modules.update(self.mc, |mods| {
            let mut mods = mods.clone();
            f(&mut mods[m as usize]);
            mods
        });
    }

    /// Adds a module, returning its id
    pub fn push_module(&self, record: &ModuleRecord<'gc>) -> u32 {
        self.realm.modules.update_with(self.mc, |mods| {
            let mut mods = mods.clone();
            mods.push(record.clone());
            let id = u32::try_from(mods.len() - 1).unwrap_or(u32::MAX);
            (mods, id)
        })
    }

    fn module_info(&self, m: u32) -> (alloc::sync::Arc<ModuleInfo>, JsEnv<'gc>, Vec<u32>) {
        let r = &self.modules()[m as usize];
        (r.info.clone(), r.env, r.requested.clone())
    }

    /// ResolveExport(module, name).
    pub fn resolve_export(&self, m: u32, name: &str, seen: &mut Vec<(u32, Box<str>)>) -> Resolution<'gc> {
        if seen.iter().any(|(x, n)| *x == m && **n == *name) {
            return Resolution::NotFound;
        }
        seen.push((m, name.into()));
        let (info, env, requested) = self.module_info(m);
        if let Some(&(_, slot)) = info.local_exports.iter().find(|(n, _)| **n == *name) {
            return Resolution::Found(ResolvedBinding::Slot(env, slot));
        }
        if let Some((_, req, import)) = info.indirect_exports.iter().find(|(n, _, _)| **n == *name) {
            let target = requested[*req];
            return match import {
                ImportName::Namespace => Resolution::Found(ResolvedBinding::Namespace(target)),
                ImportName::Named(n) => self.resolve_export(target, n, seen),
            };
        }
        if name == "default" {
            return Resolution::NotFound;
        }
        let mut found: Option<ResolvedBinding<'gc>> = None;
        for &req in &info.star_exports {
            match self.resolve_export(requested[req], name, seen) {
                Resolution::Ambiguous => return Resolution::Ambiguous,
                Resolution::NotFound => {}
                Resolution::Found(b) => match found {
                    None => found = Some(b),
                    Some(f) if f.same(b) => {}
                    Some(_) => return Resolution::Ambiguous,
                },
            }
        }
        found.map_or(Resolution::NotFound, Resolution::Found)
    }

    /// GetExportedNames(module).
    pub fn exported_names(&self, m: u32, seen: &mut Vec<u32>) -> Vec<Box<str>> {
        if seen.contains(&m) {
            return Vec::new();
        }
        seen.push(m);
        let (info, _, requested) = self.module_info(m);
        let mut out: Vec<Box<str>> = info.local_exports.iter().map(|(n, _)| n.clone()).chain(info.indirect_exports.iter().map(|(n, _, _)| n.clone())).collect();
        for &req in &info.star_exports {
            for n in self.exported_names(requested[req], seen) {
                if &*n != "default" && !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        out
    }

    /// The binding's current value (throws a ReferenceError if not yet initialized).
    pub fn binding_value(&mut self, b: ResolvedBinding<'gc>, name: &str) -> Result<Value<'gc>, Error> {
        match b {
            ResolvedBinding::Slot(env, slot) => match env.load(usize::from(slot)).kind() {
                ValueKind::Uninit => Err(self.throw(ErrorKind::Reference, &alloc::format!("Cannot access '{name}' before initialization"))),
                v => Ok(v.into()),
            },
            ResolvedBinding::Namespace(m) => Ok(self.module_namespace(m).into()),
        }
    }

    /// The module's namespace object (created on first request, recording export names in sorted order with their
    /// resolutions).
    pub fn module_namespace(&mut self, m: u32) -> JsObject<'gc> {
        if let Some(ns) = self.modules()[m as usize].namespace {
            return ns;
        }
        let mut names = self.exported_names(m, &mut Vec::new());
        names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        let mut exports = Vec::new();
        for n in names {
            if let Resolution::Found(b) = self.resolve_export(m, &n, &mut Vec::new()) {
                exports.push((self.string(&n), b));
            }
        }
        let ns = crate::ObjectData { kind: ObjectKind::ModuleNamespace(exports), proto: None, non_extensible: true, ..crate::ObjectData::default() }.alloc(self.mc);
        let tag = self.string("Module");
        ns.borrow_mut(self.mc).define_symbol(self.realm.symbols.to_string_tag, Property { slot: Slot::Data(tag.into()), enumerable: false, writable: false, configurable: false });
        self.update_module(m, |r| r.namespace = Some(ns));
        ns
    }

    /// An export on a namespace object (`None` if not a namespace object or not a string key).
    #[expect(clippy::option_option, reason = "outer: whether it is a string key on a namespace object; inner: whether that export exists")]
    pub(crate) fn namespace_export(o: JsObject<'gc>, key: Key<'_, 'gc>) -> Option<Option<ResolvedBinding<'gc>>> {
        let Key::Str(k) = key else { return None };
        if o.is_plain() {
            return None;
        }
        match &o.borrow().kind {
            ObjectKind::ModuleNamespace(exports) => Some(exports.iter().find(|(n, _)| ***n == *k).map(|&(_, b)| b)),
            _ => None,
        }
    }

    /// [[GetOwnProperty]] of a namespace object (string keys).
    pub(crate) fn namespace_own(&mut self, b: Option<ResolvedBinding<'gc>>, key: &str) -> Result<Option<Property<'gc>>, Error> {
        let Some(b) = b else { return Ok(None) };
        let v = self.binding_value(b, key)?;
        Ok(Some(Property { slot: Slot::Data(v), enumerable: true, writable: true, configurable: false }))
    }

    /// Own keys of a namespace object: export names (sorted), then Symbol keys.
    pub(crate) fn namespace_keys(o: JsObject<'gc>) -> Option<Vec<PropertyKey<'gc>>> {
        let d = o.borrow();
        let ObjectKind::ModuleNamespace(exports) = &d.kind else { return None };
        let mut keys: Vec<PropertyKey<'gc>> = exports.iter().map(|&(n, _)| PropertyKey::String(n)).collect();
        keys.extend(d.symbols.iter().map(|&(s, _)| PropertyKey::Symbol(s)));
        Some(keys)
    }
}
