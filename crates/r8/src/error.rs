use std::fmt;

use crate::{Tier, Value};

/// 框架与各执行层共用的错误。
#[derive(Debug)]
pub enum Error {
    /// 源码读不进来，带位置
    Syntax(String),
    /// 这个执行层还不支持的写法，带位置
    Unsupported { tier: Tier, what: String },
    /// JS 抛出的异常
    Exception(Value),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syntax(m) => write!(f, "syntax error: {m}"),
            Error::Unsupported { tier, what } => write!(f, "{}: unsupported: {what}", tier.name()),
            Error::Exception(v) => write!(f, "uncaught exception: {v:?}"),
        }
    }
}

impl std::error::Error for Error {}
