use alloc::string::String;
use core::fmt;

use crate::Global;

/// The error shared by the framework and all execution layers.
#[derive(Debug)]
pub enum Error {
    /// The source couldn't be read, with a location
    Syntax(String),
    /// Syntax not supported yet, with a location
    Unsupported(String),
    /// An exception thrown by JS (the value is stored as a persistent handle, so the error can be carried out of
    /// `enter`)
    Exception(Global),
    /// 被 [`IsolateHandle::terminate`](crate::IsolateHandle::terminate) 中止：JS 接不住，传到最外层
    Terminated,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syntax(m) => write!(f, "syntax error: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::Exception(_) => f.write_str("uncaught exception"),
            Error::Terminated => f.write_str("execution terminated"),
        }
    }
}

impl core::error::Error for Error {}
