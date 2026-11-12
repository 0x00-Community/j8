use alloc::string::String;
use core::fmt;

use crate::Global;

/// 框架与各执行层共用的错误。
#[derive(Debug)]
pub enum Error {
    /// 源码读不进来，带位置
    Syntax(String),
    /// 还不支持的写法，带位置
    Unsupported(String),
    /// JS 抛出的异常（值存成持久句柄，错误能带出 `enter`）
    Exception(Global),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syntax(m) => write!(f, "syntax error: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::Exception(_) => f.write_str("uncaught exception"),
        }
    }
}

impl core::error::Error for Error {}
