use std::marker::PhantomData;

use crate::{Isolate, Value};

/// 句柄作用域：其中建的 [`Local`] 活到 `'s`（作用域本身的借用期）。
pub struct HandleScope<'s, 'p> {
    isolate: &'s mut Isolate<'p>,
    base: usize,
}

/// 栈上句柄：作用域里的一个槽位，作用域结束即失效。
#[derive(Clone, Copy, Debug)]
pub struct Local<'s> {
    slot: u32,
    scope: PhantomData<&'s ()>,
}

/// 持久句柄：活到显式 [`Isolate::release`] 为止。
#[derive(Debug)]
pub struct Global {
    slot: u32,
}

impl<'s, 'p> HandleScope<'s, 'p> {
    pub(crate) fn new(isolate: &'s mut Isolate<'p>) -> Self {
        let base = isolate.locals.len();
        Self { isolate, base }
    }

    pub fn isolate(&mut self) -> &mut Isolate<'p> {
        self.isolate
    }

    pub fn local(&mut self, v: Value) -> Local<'s> {
        self.isolate.locals.push(v);
        Local { slot: u32::try_from(self.isolate.locals.len() - 1).expect("fewer than 2^32 locals"), scope: PhantomData }
    }

    pub fn get(&self, l: Local<'s>) -> Value {
        self.isolate.locals[l.slot as usize]
    }

    /// 把一个栈上句柄升成持久句柄。
    pub fn global(&mut self, l: Local<'s>) -> Global {
        let v = self.get(l);
        self.isolate.hold(v)
    }
}

impl Drop for HandleScope<'_, '_> {
    fn drop(&mut self) {
        self.isolate.locals.truncate(self.base);
    }
}

impl Isolate<'_> {
    pub fn hold(&mut self, v: Value) -> Global {
        let slot = if let Some(i) = self.globals.iter().position(Option::is_none) {
            self.globals[i] = Some(v);
            i
        } else {
            self.globals.push(Some(v));
            self.globals.len() - 1
        };
        Global { slot: u32::try_from(slot).expect("fewer than 2^32 globals") }
    }

    pub fn resolve(&self, g: &Global) -> Value {
        self.globals[g.slot as usize].expect("a live global handle")
    }

    /// 收走句柄本身，放掉之后不能再用。
    #[expect(clippy::needless_pass_by_value, reason = "按值收走句柄，防止放掉后再用")]
    pub fn release(&mut self, g: Global) {
        self.globals[g.slot as usize] = None;
    }
}
