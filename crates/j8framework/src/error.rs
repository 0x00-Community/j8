use alloc::string::String;
use core::fmt;

#[derive(Debug)]
pub enum Error {
    Syntax(String),
    Unsupported(String),
    Exception,
    Terminated,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syntax(m) => write!(f, "syntax error: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::Exception => f.write_str("uncaught exception"),
            Error::Terminated => f.write_str("execution terminated"),
        }
    }
}

impl core::error::Error for Error {}
