//! What the compiler produces, shaped as V8's: a [`SharedFunction`] (V8's `SharedFunctionInfo`) holds a
//! [`BytecodeArray`] (code, constant pool, handler table, frame size, feedback metadata); a [`Module`] is every function
//! compiled from one source, the first being the top level. The `Flag8` operands' layouts follow V8's
//! `bytecode-flags-and-tokens.h`.
use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::{FeedbackSlotKind, FunctionKind, ScopeType, VariableMode};

/// A constant pool entry (the kinds V8's bytecode generator puts there).
#[derive(Clone, Debug)]
pub enum Constant {
    /// A heap number (not a Smi)
    Number(f64),
    /// An internalized string: property names, string literals
    String(Box<str>),
    /// Decimal digits
    BigInt(Box<str>),
    /// A function to create a closure of (`CreateClosure`): its index in the [`Module`]
    Function(u32),
    /// Describes a context to create (`Create*Context`)
    Scope(ScopeInfo),
    /// An object literal's boilerplate (`CreateObjectLiteral`)
    Object(ObjectBoilerplate),
    /// An array literal's boilerplate (`CreateArrayLiteral`)
    Array(ArrayBoilerplate),
    /// A tagged template's strings (`GetTemplateObject`)
    Template(TemplateObject),
    /// A class's methods and accessors (`CallRuntime [DefineClass]`)
    Class(ClassBoilerplate),
    /// A script's or eval code's global declarations (`CallRuntime [DeclareGlobals]`): names, with the function index
    /// for function declarations
    Declarations(Vec<GlobalDeclaration>),
    /// Jump tables (`SwitchOnSmiNoFeedback`, `SwitchOnGeneratorState`) and far jumps (`Jump*Constant`): a bytecode
    /// offset
    Offset(u32),
    /// `Symbol.asyncIterator` (V8's `AsyncIteratorSymbol` constant): `for await` gets the async iterator method by it
    AsyncIteratorSymbol,
    /// An empty slot reserved for a value filled in later (V8's `InsertEntry` placeholder); never left in finished code
    Reserved,
}

impl PartialEq for Constant {
    /// Numbers compare bitwise (`0` and `-0` stay apart), so the constant pool can share entries
    fn eq(&self, o: &Self) -> bool {
        match (self, o) {
            (Self::Number(a), Self::Number(b)) => a.to_bits() == b.to_bits(),
            (Self::String(a), Self::String(b)) | (Self::BigInt(a), Self::BigInt(b)) => a == b,
            (Self::Function(a), Self::Function(b)) | (Self::Offset(a), Self::Offset(b)) => a == b,
            _ => false,
        }
    }
}

/// One global declaration: a `var` (no function) or a function declaration (its [`Module`] index).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalDeclaration {
    pub name: Box<str>,
    pub function: Option<u32>,
}

/// A context's layout (V8's `ScopeInfo`, the part j8 uses): its kind and the variables in its slots. Slot `i` of the
/// context holds `variables[i - Context::MIN_SLOTS]`.
#[derive(Clone, Debug, Default)]
pub struct ScopeInfo {
    pub scope_type: Option<ScopeType>,
    pub variables: Vec<ContextVariable>,
    /// Sloppy functions with a direct eval: the context has an extension slot for eval-declared `var`s
    pub has_extension: bool,
}

/// A variable in a context slot.
#[derive(Clone, Debug)]
pub struct ContextVariable {
    pub name: Box<str>,
    pub mode: VariableMode,
}

/// V8's `Context` layout: slot 0 the scope info, slot 1 the previous context, then the variables (after an extension
/// slot when the scope has one).
pub struct Context;

impl Context {
    pub const SCOPE_INFO_INDEX: u32 = 0;
    pub const PREVIOUS_INDEX: u32 = 1;
    pub const EXTENSION_INDEX: u32 = 2;
    /// Slots before the variables
    pub const MIN_SLOTS: u32 = 2;
    pub const MIN_SLOTS_WITH_EXTENSION: u32 = 3;
}

/// One literal property: a constant key and a constant value (`None`: computed at run time, the boilerplate holds
/// `undefined` there).
#[derive(Clone, Debug)]
pub struct ObjectBoilerplate {
    pub properties: Vec<(Box<str>, Option<LiteralValue>)>,
}

/// An array literal's elements (`None`: computed at run time; holes are `Some(LiteralValue::Hole)`).
#[derive(Clone, Debug)]
pub struct ArrayBoilerplate {
    pub elements: Vec<Option<LiteralValue>>,
}

/// A value a boilerplate can hold.
#[derive(Clone, Debug, PartialEq)]
pub enum LiteralValue {
    Number(f64),
    String(Box<str>),
    Bool(bool),
    Null,
    Undefined,
    Hole,
}

/// A tagged template's strings: cooked (`None` where the escape is invalid) and raw.
#[derive(Clone, Debug)]
pub struct TemplateObject {
    pub cooked: Vec<Option<Box<str>>>,
    pub raw: Vec<Box<str>>,
}

/// A class's members known at compile time (V8's `ClassBoilerplate`): constant-named methods and accessors on the
/// prototype and on the constructor; computed ones are defined at run time.
#[derive(Clone, Debug, Default)]
pub struct ClassBoilerplate {
    pub instance: Vec<ClassMember>,
    pub statics: Vec<ClassMember>,
}

/// A method (`Function` constant index) or accessor of a class.
#[derive(Clone, Debug)]
pub struct ClassMember {
    pub name: Box<str>,
    pub kind: MemberKind,
    /// Constant pool index of the function
    pub function: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberKind {
    Method,
    Getter,
    Setter,
}

/// A handler table entry (V8's `HandlerTable` for bytecode): a `try` range, where to go, the register the context
/// was saved in, and how the exception is predicted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandlerEntry {
    pub start: u32,
    pub end: u32,
    pub handler: u32,
    pub context: i32,
    pub prediction: CatchPrediction,
}

/// V8's `HandlerTable::CatchPrediction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatchPrediction {
    Uncaught,
    Caught,
    Promise,
    AsyncAwait,
    UncaughtAsyncAwait,
}

/// A function's bytecode (V8's `BytecodeArray`).
#[derive(Clone, Debug, Default)]
pub struct BytecodeArray {
    pub code: Box<[u8]>,
    pub constants: Vec<Constant>,
    pub handlers: Vec<HandlerEntry>,
    /// Parameters including the receiver
    pub parameter_count: u16,
    /// Registers r0 … (the frame's register file)
    pub register_count: u32,
    /// The feedback vector's layout: a slot's kind, then `slot_size - 1` `Invalid` entries
    pub feedback: Vec<FeedbackSlotKind>,
    /// The register holding `new.target` / the generator object, when the function uses it (V8's
    /// `incoming_new_target_or_generator_register`)
    pub new_target_or_generator: Option<i32>,
}

/// A function (V8's `SharedFunctionInfo`, the part j8 uses).
#[derive(Clone, Debug)]
pub struct SharedFunction {
    pub name: Box<str>,
    pub kind: FunctionKind,
    pub strict: bool,
    /// The function's `length`
    pub length: u16,
    /// Formal parameters (V8's `formal_parameter_count`, the receiver not counted)
    pub formal_parameters: u16,
    /// Its own scope, when it needs a function context
    pub scope: Option<ScopeInfo>,
    pub bytecode: BytecodeArray,
}

/// Every function compiled from one source; function 0 is the top level.
#[derive(Clone, Debug, Default)]
pub struct Module {
    pub functions: Vec<SharedFunction>,
}

impl Module {
    pub const MAIN: u32 = 0;
}

/// `CreateArrayLiteral`'s flags (V8's `CreateArrayLiteralFlags`): runtime flags in bits 0–4, fast shallow clone in
/// bit 5.
pub struct CreateArrayLiteralFlags;

impl CreateArrayLiteralFlags {
    pub const FLAGS_MASK: u8 = 0x1f;
    pub const FAST_CLONE_SUPPORTED: u8 = 1 << 5;

    pub const fn encode(fast_clone: bool, runtime_flags: u8) -> u8 {
        (runtime_flags & Self::FLAGS_MASK) | if fast_clone { Self::FAST_CLONE_SUPPORTED } else { 0 }
    }
}

/// `CreateObjectLiteral`'s flags (V8's `CreateObjectLiteralFlags`): same layout as arrays'.
pub struct CreateObjectLiteralFlags;

impl CreateObjectLiteralFlags {
    pub const FLAGS_MASK: u8 = 0x1f;
    pub const FAST_CLONE_SUPPORTED: u8 = 1 << 5;

    pub const fn encode(runtime_flags: u8, fast_clone: bool) -> u8 {
        (runtime_flags & Self::FLAGS_MASK) | if fast_clone { Self::FAST_CLONE_SUPPORTED } else { 0 }
    }
}

/// V8's `AggregateLiteral::Flags` (the runtime flags in the literal bytecodes' flag operand).
pub struct LiteralFlags;

impl LiteralFlags {
    pub const NO_FLAGS: u8 = 0;
    pub const IS_SHALLOW: u8 = 1;
    pub const DISABLE_MEMENTOS: u8 = 1 << 1;
    pub const NEEDS_INITIAL_ALLOCATION_SITE: u8 = 1 << 2;
    pub const IS_BREAK_AT_ENTRY: u8 = 1 << 3;
    /// Object literals: the boilerplate has a null prototype (`__proto__: null`)
    pub const HAS_NULL_PROTOTYPE: u8 = 1 << 3;
}

/// `CreateClosure`'s flags (V8's `CreateClosureFlags`): pretenured in bit 0, fast new closure in bit 1.
pub struct CreateClosureFlags;

impl CreateClosureFlags {
    pub const PRETENURED: u8 = 1;
    pub const FAST_NEW_CLOSURE: u8 = 1 << 1;

    pub const fn encode(pretenure: bool, is_function_scope: bool) -> u8 {
        let fast = !pretenure && is_function_scope;
        (if pretenure { Self::PRETENURED } else { 0 }) | if fast { Self::FAST_NEW_CLOSURE } else { 0 }
    }
}

/// `TestTypeOf`'s flag (V8's `TestTypeOfFlags::LiteralFlag`): which `typeof` string is compared with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TypeOfLiteral {
    Number,
    String,
    Symbol,
    Boolean,
    BigInt,
    Undefined,
    Function,
    Object,
    Other,
}

impl From<&str> for TypeOfLiteral {
    fn from(s: &str) -> Self {
        match s {
            "number" => Self::Number,
            "string" => Self::String,
            "symbol" => Self::Symbol,
            "boolean" => Self::Boolean,
            "bigint" => Self::BigInt,
            "undefined" => Self::Undefined,
            "function" => Self::Function,
            "object" => Self::Object,
            _ => Self::Other,
        }
    }
}

/// `StaLookupSlot`'s flags (V8's `StoreLookupSlotFlags`): strict in bit 0, lookup hoisting in bit 1.
pub struct StoreLookupSlotFlags;

impl StoreLookupSlotFlags {
    pub const STRICT: u8 = 1;
    pub const LOOKUP_HOISTING: u8 = 1 << 1;
}

/// V8's `TryFinallyContinuationToken`: fallthrough is -1 (the jump table's default), rethrow 0, the rest from 1.
pub struct ContinuationToken;

impl ContinuationToken {
    pub const FALLTHROUGH: i32 = -1;
    pub const RETHROW: i32 = 0;
}
