use crate::{JsStr, Mutation, Object, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Lazy {
    FunctionName,
}

impl Lazy {
    const ALL: [Self; 1] = [Self::FunctionName];

    pub fn possible(name: &str) -> bool {
        Self::ALL.iter().any(|k| k.key() == name)
    }

    const fn key(self) -> &'static str {
        match self {
            Self::FunctionName => "name",
        }
    }

    pub fn resolve<'scope>(self, holder: &Object<'scope>, mc: &Mutation<'scope>) -> Value<'scope> {
        let Some(info) = holder.closure().and_then(|c| c.code).map(|c| unsafe { c.info() }) else { return Value::Undefined };
        match self {
            Self::FunctionName => Value::String(JsStr::alloc(mc, &info.name)),
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
