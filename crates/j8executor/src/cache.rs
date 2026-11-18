//! [`CodeCache`]: bytecode shared by executors. Many isolates run the same preludes and import the same module files;
//! with a cache, each such source is parsed and compiled once, and every executor decodes its own copy. Only what is
//! meant to be shared goes in (scripts run as [`ScriptKind::Shared`](j8framework::ScriptKind::Shared), and ES
//! modules), so one-off scripts don't pile up.
//!
//! No source text is kept: an entry is found by file name, length and a 128-bit hash of the text (two randomly keyed
//! SipHash, so collisions can't be crafted), and holds the bytecode in its binary form (`j8bytecode::Binary`).
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::hash::BuildHasher;
use std::hash::RandomState;

use j8bytecode::Module;
use j8compiler::Compile;
use j8framework::{Error, ModuleInfo, Source};
use j8lockfree::Chain;

/// What a source is looked up by
#[derive(PartialEq, Eq)]
struct Key {
    file: Box<str>,
    len: usize,
    hash: [u64; 2],
}

/// One compiled source: the bytecode's binary form, plus `T` (the module info for an ES module)
struct Entry<T> {
    key: Key,
    code: Box<[u8]>,
    info: T,
}

impl<T> Entry<T> {
    /// A copy of the bytecode
    fn module(&self) -> Module {
        Module::try_from(&*self.code).expect("the cache only holds bytecode it encoded itself")
    }
}

/// Compiled sources by file name and text, shared by executors (lock-free: a source compiled by two threads at once
/// is kept twice, harmlessly)
#[derive(Default)]
pub struct CodeCache {
    seeds: [RandomState; 2],
    scripts: Chain<Entry<()>>,
    modules: Chain<Entry<Arc<ModuleInfo>>>,
}

impl CodeCache {
    /// A new cache, to hand to executors with [`Executor::with_cache`](crate::Executor::with_cache)
    pub fn shared() -> Arc<Self> {
        Arc::default()
    }

    /// The compiled classic script `src`
    pub fn script(&self, src: &Source<'_>) -> Result<Module, Error> {
        let key = self.key(src);
        if let Some(e) = self.scripts.iter().find(|e| e.key == key) {
            return Ok(e.module());
        }
        let module = Module::compile(src, j8framework::CodeKind::Script)?;
        self.scripts.push(Entry { key, code: Vec::from(&module).into(), info: () });
        Ok(module)
    }

    /// The compiled ES module `src`, with its module info
    pub fn module(&self, src: &Source<'_>) -> Result<(Module, Arc<ModuleInfo>), Error> {
        let key = self.key(src);
        if let Some(e) = self.modules.iter().find(|e| e.key == key) {
            return Ok((e.module(), Arc::clone(&e.info)));
        }
        let (module, info) = Module::compile_module(src)?;
        let info = Arc::new(info);
        self.modules.push(Entry { key, code: Vec::from(&module).into(), info: Arc::clone(&info) });
        Ok((module, info))
    }

    fn key(&self, src: &Source<'_>) -> Key {
        Key { file: src.file.into(), len: src.text.len(), hash: self.seeds.each_ref().map(|s| s.hash_one(src.text)) }
    }
}
