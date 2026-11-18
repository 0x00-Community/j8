//! [`CodeCache`]: bytecode shared by executors. Many isolates run the same preludes and import the same module files;
//! with a cache, each such source is parsed and compiled once, and every executor loads a copy of the result. Only
//! what is meant to be shared goes in (scripts run as [`ScriptKind::Shared`](j8framework::ScriptKind::Shared), and ES
//! modules), so one-off scripts don't pile up.
use alloc::boxed::Box;
use alloc::sync::Arc;

use j8framework::{Error, ModuleInfo, Source};
use j8lockfree::Chain;
use j8bytecode::Module;
use j8compiler::Compile;

/// One compiled source
struct Entry<T> {
    file: Box<str>,
    text: Box<str>,
    code: T,
}

/// Compiled sources by file name and text, shared by executors (lock-free: a source compiled by two threads at once
/// is kept twice, harmlessly)
#[derive(Default)]
pub struct CodeCache {
    scripts: Chain<Entry<Module>>,
    modules: Chain<Entry<(Module, ModuleInfo)>>,
}

impl CodeCache {
    /// A new cache, to hand to executors with [`Executor::with_cache`](crate::Executor::with_cache)
    pub fn shared() -> Arc<Self> {
        Arc::default()
    }

    /// The compiled classic script `src`
    pub fn script(&self, src: &Source<'_>) -> Result<Module, Error> {
        if let Some(e) = Self::find(&self.scripts, src) {
            return Ok(e.code.clone());
        }
        let code = Module::compile(src, j8framework::CodeKind::Script)?;
        Ok(self.scripts.push(Entry { file: src.file.into(), text: src.text.into(), code }).code.clone())
    }

    /// The compiled ES module `src`, with its module info
    pub fn module(&self, src: &Source<'_>) -> Result<(Module, ModuleInfo), Error> {
        if let Some(e) = Self::find(&self.modules, src) {
            return Ok(e.code.clone());
        }
        let code = Module::compile_module(src)?;
        Ok(self.modules.push(Entry { file: src.file.into(), text: src.text.into(), code }).code.clone())
    }

    fn find<'c, T>(entries: &'c Chain<Entry<T>>, src: &Source<'_>) -> Option<&'c Entry<T>> {
        entries.iter().find(|e| e.text.len() == src.text.len() && *e.file == *src.file && *e.text == *src.text)
    }
}
