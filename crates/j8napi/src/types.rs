use core::ffi::{c_char, c_void};
use core::num::NonZeroUsize;

use crate::Env;

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok = 0,
    InvalidArg = 1,
    ObjectExpected = 2,
    StringExpected = 3,
    NameExpected = 4,
    FunctionExpected = 5,
    NumberExpected = 6,
    BooleanExpected = 7,
    ArrayExpected = 8,
    GenericFailure = 9,
    PendingException = 10,
    Cancelled = 11,
    EscapeCalledTwice = 12,
    HandleScopeMismatch = 13,
    CallbackScopeMismatch = 14,
    QueueFull = 15,
    Closing = 16,
    BigintExpected = 17,
    DateExpected = 18,
    ArraybufferExpected = 19,
    DetachableArraybufferExpected = 20,
    WouldDeadlock = 21,
    NoExternalBuffersAllowed = 22,
    CannotRunJs = 23,
}

impl Status {
    pub fn message(self) -> &'static core::ffi::CStr {
        match self {
            Self::Ok => c"",
            Self::InvalidArg => c"Invalid argument",
            Self::ObjectExpected => c"An object was expected",
            Self::StringExpected => c"A string was expected",
            Self::NameExpected => c"A string or symbol was expected",
            Self::FunctionExpected => c"A function was expected",
            Self::NumberExpected => c"A number was expected",
            Self::BooleanExpected => c"A boolean was expected",
            Self::ArrayExpected => c"An array was expected",
            Self::GenericFailure => c"Unknown failure",
            Self::PendingException => c"An exception is pending",
            Self::Cancelled => c"The async work item was cancelled",
            Self::EscapeCalledTwice => c"napi_escape_handle already called on scope",
            Self::HandleScopeMismatch => c"Invalid handle scope usage",
            Self::CallbackScopeMismatch => c"Invalid callback scope usage",
            Self::QueueFull => c"Thread-safe function queue is full",
            Self::Closing => c"Thread-safe function handle is closing",
            Self::BigintExpected => c"A bigint was expected",
            Self::DateExpected => c"A date was expected",
            Self::ArraybufferExpected => c"An arraybuffer was expected",
            Self::DetachableArraybufferExpected => c"A detachable arraybuffer was expected",
            Self::WouldDeadlock => c"Main thread would deadlock",
            Self::NoExternalBuffersAllowed => c"External buffers are not allowed",
            Self::CannotRunJs => c"Cannot run JavaScript",
        }
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Handle(pub Option<NonZeroUsize>);

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueType {
    Undefined = 0,
    Null = 1,
    Boolean = 2,
    Number = 3,
    String = 4,
    Symbol = 5,
    Object = 6,
    Function = 7,
    External = 8,
    Bigint = 9,
}

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedArrayType {
    Int8 = 0,
    Uint8 = 1,
    Uint8Clamped = 2,
    Int16 = 3,
    Uint16 = 4,
    Int32 = 5,
    Uint32 = 6,
    Float32 = 7,
    Float64 = 8,
    BigInt64 = 9,
    BigUint64 = 10,
    Float16 = 11,
}

impl From<TypedArrayType> for j8framework::ElementKind {
    fn from(t: TypedArrayType) -> Self {
        match t {
            TypedArrayType::Int8 => Self::Int8,
            TypedArrayType::Uint8 => Self::Uint8,
            TypedArrayType::Uint8Clamped => Self::Uint8Clamped,
            TypedArrayType::Int16 => Self::Int16,
            TypedArrayType::Uint16 => Self::Uint16,
            TypedArrayType::Int32 => Self::Int32,
            TypedArrayType::Uint32 => Self::Uint32,
            TypedArrayType::Float32 => Self::Float32,
            TypedArrayType::Float64 => Self::Float64,
            TypedArrayType::BigInt64 => Self::BigInt64,
            TypedArrayType::BigUint64 => Self::BigUint64,
            TypedArrayType::Float16 => Self::Float16,
        }
    }
}

impl From<j8framework::ElementKind> for TypedArrayType {
    fn from(k: j8framework::ElementKind) -> Self {
        use j8framework::ElementKind;
        match k {
            ElementKind::Int8 => Self::Int8,
            ElementKind::Uint8 => Self::Uint8,
            ElementKind::Uint8Clamped => Self::Uint8Clamped,
            ElementKind::Int16 => Self::Int16,
            ElementKind::Uint16 => Self::Uint16,
            ElementKind::Int32 => Self::Int32,
            ElementKind::Uint32 => Self::Uint32,
            ElementKind::Float32 => Self::Float32,
            ElementKind::Float64 => Self::Float64,
            ElementKind::BigInt64 => Self::BigInt64,
            ElementKind::BigUint64 => Self::BigUint64,
            ElementKind::Float16 => Self::Float16,
        }
    }
}

impl TryFrom<i32> for TypedArrayType {
    type Error = Status;

    fn try_from(n: i32) -> Result<Self, Status> {
        Ok(match n {
            0 => Self::Int8,
            1 => Self::Uint8,
            2 => Self::Uint8Clamped,
            3 => Self::Int16,
            4 => Self::Uint16,
            5 => Self::Int32,
            6 => Self::Uint32,
            7 => Self::Float32,
            8 => Self::Float64,
            9 => Self::BigInt64,
            10 => Self::BigUint64,
            11 => Self::Float16,
            _ => return Err(Status::InvalidArg),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct Attributes(pub i32);

impl Attributes {
    pub const WRITABLE: i32 = 1 << 0;
    pub const ENUMERABLE: i32 = 1 << 1;
    pub const CONFIGURABLE: i32 = 1 << 2;
    pub const STATIC: i32 = 1 << 10;

    pub fn has(self, bit: i32) -> bool {
        self.0 & bit != 0
    }
}

pub type Callback = Option<unsafe extern "C" fn(env: *mut Env, info: *mut crate::CallbackInfo) -> Handle>;

pub type Finalize = Option<unsafe extern "C" fn(env: *mut Env, data: *mut c_void, hint: *mut c_void)>;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PropertyDescriptor {
    pub utf8name: *const c_char,
    pub name: Handle,
    pub method: Callback,
    pub getter: Callback,
    pub setter: Callback,
    pub value: Handle,
    pub attributes: Attributes,
    pub data: *mut c_void,
}

#[repr(C)]
#[derive(Debug)]
pub struct ExtendedErrorInfo {
    pub error_message: *const c_char,
    pub engine_reserved: *mut c_void,
    pub engine_error_code: u32,
    pub error_code: Status,
}

impl Default for ExtendedErrorInfo {
    fn default() -> Self {
        Self { error_message: core::ptr::null(), engine_reserved: core::ptr::null_mut(), engine_error_code: 0, error_code: Status::Ok }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypeTag {
    pub lower: u64,
    pub upper: u64,
}

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyMode {
    IncludePrototypes = 0,
    OwnOnly = 1,
}

impl TryFrom<i32> for KeyMode {
    type Error = Status;

    fn try_from(n: i32) -> Result<Self, Status> {
        match n {
            0 => Ok(Self::IncludePrototypes),
            1 => Ok(Self::OwnOnly),
            _ => Err(Status::InvalidArg),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct KeyFilter(pub i32);

impl KeyFilter {
    pub const WRITABLE: i32 = 1 << 0;
    pub const ENUMERABLE: i32 = 1 << 1;
    pub const CONFIGURABLE: i32 = 1 << 2;
    pub const SKIP_STRINGS: i32 = 1 << 3;
    pub const SKIP_SYMBOLS: i32 = 1 << 4;

    pub fn has(self, bit: i32) -> bool {
        self.0 & bit != 0
    }
}

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyConversion {
    KeepNumbers = 0,
    NumbersToStrings = 1,
}

pub type CleanupHook = unsafe extern "C" fn(arg: *mut c_void);

#[repr(C)]
#[derive(Debug)]
pub struct NodeVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub release: *const c_char,
}

impl NodeVersion {
    pub const MAJOR: u32 = 24;
}

impl Default for NodeVersion {
    fn default() -> Self {
        Self { major: Self::MAJOR, minor: 0, patch: 0, release: c"j8".as_ptr() }
    }
}

impl TryFrom<i32> for KeyConversion {
    type Error = Status;

    fn try_from(n: i32) -> Result<Self, Status> {
        match n {
            0 => Ok(Self::KeepNumbers),
            1 => Ok(Self::NumbersToStrings),
            _ => Err(Status::InvalidArg),
        }
    }
}

pub type RegisterFn = unsafe extern "C" fn(env: *mut Env, exports: Handle) -> Handle;
