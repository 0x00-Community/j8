//! Property values computed on first read (V8's `AccessorInfo` on function maps: `name` is not made when a closure
//! is): a property slot holds a [`Lazy`] word until an object-level read turns it into the value, stored back in
//! place, so the next reads (inline caches included) see plain data.
use crate::{JsStr, Mutation, Object, Value};

/// What a lazy slot computes, from the object holding it
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Lazy {
    /// A function object's `name`: its code's [`crate::FunctionInfo::name`]
    FunctionName,
}

impl Lazy {
    /// Every kind, by its number in the word
    const ALL: [Self; 1] = [Self::FunctionName];

    /// The value for `holder`
    pub fn resolve<'gc>(self, holder: &Object<'gc>, mc: &Mutation<'gc>) -> Value<'gc> {
        // SAFETY: the info is read here only, within this mutate, while the engine that made the closure runs it
        let Some(info) = holder.closure().and_then(|c| c.code).map(|c| unsafe { c.info() }) else { return Value::Undefined };
        match self {
            Self::FunctionName => Value::String(mc.alloc(JsStr::from(info.name.clone()))),
        }
    }
}

impl From<Lazy> for u64 {
    fn from(l: Lazy) -> Self {
        l as Self
    }
}

impl TryFrom<u64> for Lazy {
    type Error = u64;

    fn try_from(n: u64) -> Result<Self, u64> {
        usize::try_from(n).ok().and_then(|i| Self::ALL.get(i)).copied().ok_or(n)
    }
}
