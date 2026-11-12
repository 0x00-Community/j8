//! ES 模块：编译器产出的静态信息、运行时的模块记录、导出解析、命名空间对象的属性行为（规范 10.4.6）。
use alloc::boxed::Box;
use alloc::vec::Vec;

use r8gc::{Collect, Gc};

use crate::{Agent, Error, ErrorKind, FuncId, JsEnv, JsObject, JsString, Key, ObjectKind, Property, PropertyKey, Slot, Value};

/// 导入的是什么：某个名字，或整个命名空间（`import * as ns` / `export * as ns from`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportName {
    Named(Box<str>),
    Namespace,
}

/// 一条导入：从第 `request` 个请求的模块导入 `name`（下标就是导入绑定的编号）。
#[derive(Clone, Debug)]
pub struct ImportEntry {
    pub request: usize,
    pub name: ImportName,
}

/// 编译器给出的模块静态信息。
#[derive(Clone, Debug, Default)]
pub struct ModuleInfo {
    /// `from` 后面的模块说明符（去重，按出现顺序）
    pub requests: Vec<Box<str>>,
    pub imports: Vec<ImportEntry>,
    /// 本模块自己的导出：导出名 ⇒ 模块环境槽位
    pub local_exports: Vec<(Box<str>, u16)>,
    /// 转导出：导出名 ⇒（请求, 导入名）
    pub indirect_exports: Vec<(Box<str>, usize, ImportName)>,
    /// `export * from`
    pub star_exports: Vec<usize>,
    /// 模块环境的槽位数；哪些槽位是 `let` / `const` / `class`（开始时未初始化）
    pub slots: u16,
    pub lexical: Vec<u16>,
}

/// 模块的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Collect)]
#[collect(require_static)]
pub enum ModuleStatus {
    Linked,
    Evaluating,
    Evaluated,
}

/// 解析好的一个绑定：某个模块环境的槽位，或某个模块的命名空间对象。
#[derive(Clone, Copy, Debug, Collect)]
#[collect(no_drop)]
pub enum ResolvedBinding<'gc> {
    Slot(JsEnv<'gc>, u16),
    Namespace(u32),
}

/// 运行时的模块记录（放在 Realm 里，按加载顺序编号）。
#[derive(Collect)]
#[collect(no_drop)]
pub struct ModuleRecord<'gc> {
    pub path: JsString<'gc>,
    pub env: JsEnv<'gc>,
    #[collect(require_static)]
    pub info: alloc::rc::Rc<ModuleInfo>,
    pub main: FuncId,
    pub status: ModuleStatus,
    /// 求值抛出的异常（再导入同一个模块照样抛）
    pub error: Option<Value<'gc>>,
    /// 各请求对应的模块编号
    pub requested: Vec<u32>,
    /// 各导入绑定解析到哪里
    pub imports: Vec<ResolvedBinding<'gc>>,
    pub namespace: Option<JsObject<'gc>>,
    pub meta: Option<JsObject<'gc>>,
}

impl core::fmt::Debug for ModuleRecord<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ModuleRecord").field("path", &&**self.path).finish()
    }
}

/// ResolveExport 的结果。
#[derive(Clone, Copy, Debug)]
pub enum Resolution<'gc> {
    Found(ResolvedBinding<'gc>),
    NotFound,
    Ambiguous,
}

impl ResolvedBinding<'_> {
    /// 是不是同一个绑定。
    fn same(self, other: Self) -> bool {
        match (self, other) {
            (ResolvedBinding::Slot(e1, s1), ResolvedBinding::Slot(e2, s2)) => Gc::ptr_eq(e1, e2) && s1 == s2,
            (ResolvedBinding::Namespace(x), ResolvedBinding::Namespace(y)) => x == y,
            _ => false,
        }
    }
}

impl<'gc> Agent<'gc, '_> {
    fn module_info(&self, m: u32) -> (alloc::rc::Rc<ModuleInfo>, JsEnv<'gc>, Vec<u32>) {
        let mods = self.realm.modules.borrow();
        let r = &mods[m as usize];
        (r.info.clone(), r.env, r.requested.clone())
    }

    /// ResolveExport(module, name)。
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

    /// GetExportedNames(module)。
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

    /// 绑定的当前值（还没初始化抛 ReferenceError）。
    pub fn binding_value(&mut self, b: ResolvedBinding<'gc>, name: &str) -> Result<Value<'gc>, Error> {
        match b {
            ResolvedBinding::Slot(env, slot) => match env.borrow().slots[usize::from(slot)] {
                Value::Uninit => Err(self.throw(ErrorKind::Reference, &alloc::format!("Cannot access '{name}' before initialization"))),
                v => Ok(v),
            },
            ResolvedBinding::Namespace(m) => Ok(self.module_namespace(m).into()),
        }
    }

    /// 模块的命名空间对象（第一次要时建，导出名排好序记下解析结果）。
    pub fn module_namespace(&mut self, m: u32) -> JsObject<'gc> {
        if let Some(ns) = self.realm.modules.borrow()[m as usize].namespace {
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
        self.realm.modules.borrow_mut(self.mc)[m as usize].namespace = Some(ns);
        ns
    }

    /// 命名空间对象上的导出（不是命名空间对象或不是字符串键给 `None`）。
    #[expect(clippy::option_option, reason = "外层：是不是命名空间对象的字符串键；内层：有没有这个导出")]
    pub(crate) fn namespace_export(o: JsObject<'gc>, key: Key<'_, 'gc>) -> Option<Option<ResolvedBinding<'gc>>> {
        let Key::Str(k) = key else { return None };
        match &o.borrow().kind {
            ObjectKind::ModuleNamespace(exports) => Some(exports.iter().find(|(n, _)| ***n == *k).map(|&(_, b)| b)),
            _ => None,
        }
    }

    /// 命名空间对象的 [[GetOwnProperty]]（字符串键）。
    pub(crate) fn namespace_own(&mut self, b: Option<ResolvedBinding<'gc>>, key: &str) -> Result<Option<Property<'gc>>, Error> {
        let Some(b) = b else { return Ok(None) };
        let v = self.binding_value(b, key)?;
        Ok(Some(Property { slot: Slot::Data(v), enumerable: true, writable: true, configurable: false }))
    }

    /// 命名空间对象的自有键：导出名（已排序）再 Symbol 键。
    pub(crate) fn namespace_keys(o: JsObject<'gc>) -> Option<Vec<PropertyKey<'gc>>> {
        let d = o.borrow();
        let ObjectKind::ModuleNamespace(exports) = &d.kind else { return None };
        let mut keys: Vec<PropertyKey<'gc>> = exports.iter().map(|&(n, _)| PropertyKey::String(n)).collect();
        keys.extend(d.symbols.iter().map(|&(s, _)| PropertyKey::Symbol(s)));
        Some(keys)
    }
}
