extern crate alloc;

mod codec;
mod function;
mod v8;
mod wasm;
mod wasm_module;

pub use codec::{BytecodeError, Decoded, Decoder, Encoder, Opcode, OperandScale, OperandSize, OperandValue, Operands};
pub use v8::{Bytecode, FeedbackSlotKind, FunctionKind, ImplicitRegisterUse, Intrinsic, NativeContextIndex, OperandType, OperandTypeInfo, Register, Runtime, ScopeType, VariableMode};
pub use wasm::{WasmBytecode, WasmOperandType};
pub use function::{ArrayBoilerplate, BytecodeArray, CatchPrediction, ClassBoilerplate, ClassMember, Constant, Context, ContextVariable, EvalDeclaration, VariableRole, ContinuationToken, CreateArrayLiteralFlags, CreateClosureFlags, CreateObjectLiteralFlags, GlobalDeclaration, HandlerEntry, LiteralFlags, LiteralValue, MemberKind, Module, ObjectBoilerplate, ScopeInfo, SharedFunction, StoreLookupSlotFlags, TemplateObject, TypeOfLiteral};
pub use wasm_module::{WasmCatch, WasmCatchKind, WasmCompositeType, WasmCustom, WasmFieldType, WasmHandler, WasmStorageType, WasmSubType, WasmData, WasmElement, WasmExport, WasmExternKind, WasmFuncType, WasmFunction, WasmGlobal, WasmGlobalType, WasmHeapType, WasmImport, WasmImportKind, WasmLimits, WasmMemoryType, WasmModule, WasmRefType, WasmSegmentMode, WasmTable, WasmTableType, WasmValType};
