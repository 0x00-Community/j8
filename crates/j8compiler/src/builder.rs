//! Emitting one function's bytecode, after V8's `BytecodeArrayBuilder` / `BytecodeArrayWriter` /
//! `BytecodeRegisterAllocator` / `ConstantArrayBuilder` / `HandlerTableBuilder` / `FeedbackVectorSpec`: picks the
//! bytecode for each operation the way V8 does (short `Star`, `LdaZero`, call forms by argument count, `InvokeIntrinsic`
//! for intrinsics …), patches forward jumps (a reserved constant pool entry backs a jump whose offset doesn't fit its
//! operand), allocates registers in stack order, numbers feedback slots.
#![allow(clippy::cast_possible_wrap, reason = "constant pool and feedback indices are far below i64::MAX")]
use alloc::vec::Vec;

use j8bytecode::{Bytecode, BytecodeArray, CatchPrediction, Constant, Encoder, FeedbackSlotKind, HandlerEntry, Intrinsic, OperandScale, OperandValue, Register, Runtime, TypeOfLiteral};
use j8framework::{BinaryOp, CompareOp};

/// Consecutive registers (V8's `RegisterList`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegisterList {
    pub first: Register,
    pub count: u32,
}

impl RegisterList {
    pub const EMPTY: Self = Self { first: Register(0), count: 0 };

    pub fn at(self, i: u32) -> Register {
        Register(self.first.0 + i32::try_from(i).unwrap_or(i32::MAX))
    }
}

/// A forward jump target (V8's `BytecodeLabel`): jumps to it are patched when it is bound.
#[derive(Default)]
pub struct Label {
    bound: Option<usize>,
    /// Each jump to it: where it starts (its prefix included) and its constant pool reservation
    refs: Vec<(usize, usize)>,
}

/// A loop header (V8's `BytecodeLoopHeader`): `JumpLoop` jumps back to it.
#[derive(Clone, Copy)]
pub struct LoopHeader(usize);

/// A jump table (V8's `BytecodeJumpTable`): `SwitchOnSmiNoFeedback` / `SwitchOnGeneratorState` over `size` cases
/// starting at `case_base`, their targets in consecutive constant pool entries from `constant`.
pub struct JumpTable {
    constant: usize,
    pub size: u32,
    pub case_base: i32,
    /// Offset of the switch bytecode itself (the prefix not counted)
    switch_at: usize,
}

/// One function's bytecode in the making.
pub struct Builder {
    enc: Encoder,
    constants: Vec<Constant>,
    handlers: Vec<HandlerEntry>,
    feedback: Vec<FeedbackSlotKind>,
    parameter_count: u16,
    /// Registers r0 … `locals` - 1 are the function's own variables; temporaries follow
    locals: i32,
    next: i32,
    max: i32,
    /// Jumps whose label isn't bound yet (they keep their constant pool reservation)
    unbound: usize,
    pub new_target_or_generator: Option<i32>,
}

impl Builder {
    pub fn new(parameter_count: u16, locals: i32) -> Self {
        Self { enc: Encoder::default(), constants: Vec::new(), handlers: Vec::new(), feedback: Vec::new(), parameter_count, locals, next: locals, max: locals, unbound: 0, new_target_or_generator: None }
    }

    /// The finished bytecode
    pub fn finish(self) -> BytecodeArray {
        debug_assert_eq!(self.unbound, 0, "every label is bound");
        BytecodeArray {
            code: self.enc.code.into_boxed_slice(),
            constants: self.constants,
            handlers: self.handlers,
            parameter_count: self.parameter_count,
            register_count: u32::try_from(self.max).unwrap_or(0),
            feedback: self.feedback,
            new_target_or_generator: self.new_target_or_generator,
        }
    }

    /// Current bytecode offset
    pub fn offset(&self) -> usize {
        self.enc.code.len()
    }

    fn out(&mut self, b: Bytecode, ops: &[OperandValue]) -> usize {
        self.enc.emit(b, ops).expect("operands match the bytecode")
    }

    // ---- registers (V8's BytecodeRegisterAllocator: stack discipline) ----

    pub fn new_register(&mut self) -> Register {
        self.new_register_list(1).first
    }

    pub fn new_register_list(&mut self, count: u32) -> RegisterList {
        let first = Register(self.next);
        self.next += i32::try_from(count).unwrap_or(i32::MAX);
        self.max = self.max.max(self.next);
        RegisterList { first, count }
    }

    /// The next register to be allocated (to release back to, V8's `RegisterAllocationScope`)
    pub fn mark(&self) -> i32 {
        self.next
    }

    pub fn release_to(&mut self, mark: i32) {
        debug_assert!(mark >= self.locals && mark <= self.next);
        self.next = mark;
    }

    /// Grows the local variables by one register (only before any temporary is live)
    pub fn new_local(&mut self) -> Register {
        debug_assert_eq!(self.next, self.locals, "locals are allocated before temporaries");
        let r = Register(self.locals);
        self.locals += 1;
        self.next = self.locals;
        self.max = self.max.max(self.next);
        r
    }

    // ---- constant pool and feedback ----

    /// The pool entry of `c` (strings, numbers and functions are shared)
    pub fn constant(&mut self, c: Constant) -> usize {
        let shared = matches!(c, Constant::Number(_) | Constant::String(_) | Constant::BigInt(_) | Constant::Function(_));
        if shared && let Some(i) = self.constants.iter().position(|x| *x == c) {
            return i;
        }
        self.constants.push(c);
        self.constants.len() - 1
    }

    pub fn name(&mut self, s: &str) -> usize {
        self.constant(Constant::String(s.into()))
    }

    /// A new feedback slot of `kind` (V8's `FeedbackVectorSpec::AddSlot`)
    pub fn slot(&mut self, kind: FeedbackSlotKind) -> usize {
        let at = self.feedback.len();
        self.feedback.push(kind);
        for _ in 1..kind.slot_size() {
            self.feedback.push(FeedbackSlotKind::Invalid);
        }
        at
    }

    // ---- accumulator and registers ----

    pub fn load_smi(&mut self, v: i32) {
        if v == 0 {
            self.out(Bytecode::LdaZero, &[]);
        } else {
            self.out(Bytecode::LdaSmi, &[OperandValue::from(v)]);
        }
    }

    /// A number: a Smi when it is one (V8's `DoubleToSmiInteger`: an int32 that isn't -0), else a constant
    pub fn load_number(&mut self, v: f64) {
        #[expect(clippy::cast_possible_truncation, reason = "checked to round-trip exactly")]
        let i = v as i32;
        if f64::from(i).to_bits() == v.to_bits() {
            self.load_smi(i);
        } else {
            let k = self.constant(Constant::Number(v));
            self.out(Bytecode::LdaConstant, &[k as OperandValue]);
        }
    }

    pub fn load_string(&mut self, s: &str) {
        let k = self.name(s);
        self.out(Bytecode::LdaConstant, &[k as OperandValue]);
    }

    pub fn load_constant(&mut self, k: usize) {
        self.out(Bytecode::LdaConstant, &[k as OperandValue]);
    }

    pub fn load_undefined(&mut self) {
        self.out(Bytecode::LdaUndefined, &[]);
    }

    pub fn load_null(&mut self) {
        self.out(Bytecode::LdaNull, &[]);
    }

    pub fn load_the_hole(&mut self) {
        self.out(Bytecode::LdaTheHole, &[]);
    }

    pub fn load_bool(&mut self, b: bool) {
        self.out(if b { Bytecode::LdaTrue } else { Bytecode::LdaFalse }, &[]);
    }

    pub fn ldar(&mut self, r: Register) {
        self.out(Bytecode::Ldar, &[i32::from(r).into()]);
    }

    /// `Star`, or the short `Star0` … `Star15`
    pub fn star(&mut self, r: Register) {
        match Bytecode::short_star(r) {
            Some(b) => self.out(b, &[]),
            None => self.out(Bytecode::Star, &[i32::from(r).into()]),
        };
    }

    pub fn mov(&mut self, from: Register, to: Register) {
        if from != to {
            self.out(Bytecode::Mov, &[i32::from(from).into(), i32::from(to).into()]);
        }
    }

    // ---- globals, contexts ----

    pub fn load_global(&mut self, name: &str, inside_typeof: bool) {
        let k = self.name(name);
        let s = self.slot(if inside_typeof { FeedbackSlotKind::LoadGlobalInsideTypeof } else { FeedbackSlotKind::LoadGlobalNotInsideTypeof });
        self.out(if inside_typeof { Bytecode::LdaGlobalInsideTypeof } else { Bytecode::LdaGlobal }, &[k as OperandValue, s as OperandValue]);
    }

    pub fn store_global(&mut self, name: &str, strict: bool) {
        let k = self.name(name);
        let s = self.slot(if strict { FeedbackSlotKind::StoreGlobalStrict } else { FeedbackSlotKind::StoreGlobalSloppy });
        self.out(Bytecode::StaGlobal, &[k as OperandValue, s as OperandValue]);
    }

    /// Context slot `slot` of the context `depth` levels out from `context` (V8's `LoadContextSlot`: the
    /// current-context form for depth 0 of the current context; immutable forms for never-assigned variables)
    pub fn load_context_slot(&mut self, context: Register, slot: u32, depth: u32, immutable: bool) {
        let current = context == Register::current_context() && depth == 0;
        match (immutable, current) {
            (true, true) => self.out(Bytecode::LdaImmutableCurrentContextSlot, &[slot.into()]),
            (true, false) => self.out(Bytecode::LdaImmutableContextSlot, &[i32::from(context).into(), slot.into(), depth.into()]),
            (false, true) => self.out(Bytecode::LdaCurrentContextSlotNoCell, &[slot.into()]),
            (false, false) => self.out(Bytecode::LdaContextSlotNoCell, &[i32::from(context).into(), slot.into(), depth.into()]),
        };
    }

    pub fn store_context_slot(&mut self, context: Register, slot: u32, depth: u32) {
        if context == Register::current_context() && depth == 0 {
            self.out(Bytecode::StaCurrentContextSlotNoCell, &[slot.into()]);
        } else {
            self.out(Bytecode::StaContextSlotNoCell, &[i32::from(context).into(), slot.into(), depth.into()]);
        }
    }

    pub fn push_context(&mut self, save: Register) {
        self.out(Bytecode::PushContext, &[i32::from(save).into()]);
    }

    pub fn pop_context(&mut self, from: Register) {
        self.out(Bytecode::PopContext, &[i32::from(from).into()]);
    }

    pub fn create_block_context(&mut self, scope: usize) {
        self.out(Bytecode::CreateBlockContext, &[scope as OperandValue]);
    }

    pub fn create_function_context(&mut self, scope: usize, slots: u32) {
        self.out(Bytecode::CreateFunctionContext, &[scope as OperandValue, slots.into()]);
    }

    pub fn create_catch_context(&mut self, exception: Register, scope: usize) {
        self.out(Bytecode::CreateCatchContext, &[i32::from(exception).into(), scope as OperandValue]);
    }

    pub fn create_with_context(&mut self, object: Register, scope: usize) {
        self.out(Bytecode::CreateWithContext, &[i32::from(object).into(), scope as OperandValue]);
    }

    pub fn create_eval_context(&mut self, scope: usize, slots: u32) {
        self.out(Bytecode::CreateEvalContext, &[scope as OperandValue, slots.into()]);
    }

    // ---- lookup slots (dynamic: with / sloppy eval) ----

    pub fn load_lookup_slot(&mut self, name: &str, inside_typeof: bool) {
        let k = self.name(name);
        self.out(if inside_typeof { Bytecode::LdaLookupSlotInsideTypeof } else { Bytecode::LdaLookupSlot }, &[k as OperandValue]);
    }

    pub fn store_lookup_slot(&mut self, name: &str, flags: u8) {
        let k = self.name(name);
        self.out(Bytecode::StaLookupSlot, &[k as OperandValue, flags.into()]);
    }

    // ---- properties ----

    pub fn get_named(&mut self, object: Register, name: &str) {
        let k = self.name(name);
        let s = self.slot(FeedbackSlotKind::LoadProperty);
        self.out(Bytecode::GetNamedProperty, &[i32::from(object).into(), k as OperandValue, s as OperandValue]);
    }

    pub fn get_named_from_super(&mut self, receiver: Register, name: &str) {
        let k = self.name(name);
        let s = self.slot(FeedbackSlotKind::LoadProperty);
        self.out(Bytecode::GetNamedPropertyFromSuper, &[i32::from(receiver).into(), k as OperandValue, s as OperandValue]);
    }

    pub fn set_named(&mut self, object: Register, name: &str, strict: bool) {
        let k = self.name(name);
        let s = self.slot(if strict { FeedbackSlotKind::SetNamedStrict } else { FeedbackSlotKind::SetNamedSloppy });
        self.out(Bytecode::SetNamedProperty, &[i32::from(object).into(), k as OperandValue, s as OperandValue]);
    }

    pub fn define_named_own(&mut self, object: Register, name: &str) {
        let k = self.name(name);
        let s = self.slot(FeedbackSlotKind::DefineNamedOwn);
        self.out(Bytecode::DefineNamedOwnProperty, &[i32::from(object).into(), k as OperandValue, s as OperandValue]);
    }

    /// `object[acc]`
    pub fn get_keyed(&mut self, object: Register) {
        let s = self.slot(FeedbackSlotKind::LoadKeyed);
        self.out(Bytecode::GetKeyedProperty, &[i32::from(object).into(), s as OperandValue]);
    }

    /// `object[key] = acc`
    pub fn set_keyed(&mut self, object: Register, key: Register, strict: bool) {
        let s = self.slot(if strict { FeedbackSlotKind::SetKeyedStrict } else { FeedbackSlotKind::SetKeyedSloppy });
        self.out(Bytecode::SetKeyedProperty, &[i32::from(object).into(), i32::from(key).into(), s as OperandValue]);
    }

    pub fn define_keyed_own(&mut self, object: Register, key: Register, flags: u8) {
        let s = self.slot(FeedbackSlotKind::DefineKeyedOwn);
        self.out(Bytecode::DefineKeyedOwnProperty, &[i32::from(object).into(), i32::from(key).into(), flags.into(), s as OperandValue]);
    }

    pub fn define_keyed_own_in_literal(&mut self, object: Register, key: Register, flags: u8) {
        let s = self.slot(FeedbackSlotKind::DefineKeyedOwnPropertyInLiteral);
        self.out(Bytecode::DefineKeyedOwnPropertyInLiteral, &[i32::from(object).into(), i32::from(key).into(), flags.into(), s as OperandValue]);
    }

    pub fn store_in_array_literal(&mut self, array: Register, index: Register) {
        let s = self.slot(FeedbackSlotKind::StoreInArrayLiteral);
        self.out(Bytecode::StaInArrayLiteral, &[i32::from(array).into(), i32::from(index).into(), s as OperandValue]);
    }

    pub fn delete_property(&mut self, object: Register, strict: bool) {
        self.out(if strict { Bytecode::DeletePropertyStrict } else { Bytecode::DeletePropertySloppy }, &[i32::from(object).into()]);
    }

    // ---- operators ----

    /// `reg op acc` (V8's `BinaryOperation`: one bytecode per operator, feedback embedded)
    pub fn binary(&mut self, op: BinaryOp, reg: Register) {
        self.out(Self::binary_bytecode(op), &[i32::from(reg).into(), 0]);
    }

    /// `acc op smi` (V8's `BinaryOperationSmiLiteral`)
    pub fn binary_smi(&mut self, op: BinaryOp, smi: i32) {
        let b = match op {
            BinaryOp::Add => Bytecode::AddSmi,
            BinaryOp::Sub => Bytecode::SubSmi,
            BinaryOp::Mul => Bytecode::MulSmi,
            BinaryOp::Div => Bytecode::DivSmi,
            BinaryOp::Rem => Bytecode::ModSmi,
            BinaryOp::Exp => Bytecode::ExpSmi,
            BinaryOp::BitOr => Bytecode::BitwiseOrSmi,
            BinaryOp::BitXor => Bytecode::BitwiseXorSmi,
            BinaryOp::BitAnd => Bytecode::BitwiseAndSmi,
            BinaryOp::Shl => Bytecode::ShiftLeftSmi,
            BinaryOp::Shr => Bytecode::ShiftRightSmi,
            BinaryOp::UShr => Bytecode::ShiftRightLogicalSmi,
        };
        self.out(b, &[OperandValue::from(smi), 0]);
    }

    pub fn binary_bytecode(op: BinaryOp) -> Bytecode {
        match op {
            BinaryOp::Add => Bytecode::Add,
            BinaryOp::Sub => Bytecode::Sub,
            BinaryOp::Mul => Bytecode::Mul,
            BinaryOp::Div => Bytecode::Div,
            BinaryOp::Rem => Bytecode::Mod,
            BinaryOp::Exp => Bytecode::Exp,
            BinaryOp::BitOr => Bytecode::BitwiseOr,
            BinaryOp::BitXor => Bytecode::BitwiseXor,
            BinaryOp::BitAnd => Bytecode::BitwiseAnd,
            BinaryOp::Shl => Bytecode::ShiftLeft,
            BinaryOp::Shr => Bytecode::ShiftRight,
            BinaryOp::UShr => Bytecode::ShiftRightLogical,
        }
    }

    /// `reg op acc` for comparisons (V8's `CompareOperation`; `!=` / `!==` are a test then `LogicalNot`, as V8's
    /// generator does)
    pub fn compare(&mut self, op: CompareOp, reg: Register) {
        let (b, negate) = match op {
            CompareOp::Eq => (Bytecode::TestEqual, false),
            CompareOp::Ne => (Bytecode::TestEqual, true),
            CompareOp::StrictEq => (Bytecode::TestEqualStrict, false),
            CompareOp::StrictNe => (Bytecode::TestEqualStrict, true),
            CompareOp::Lt => (Bytecode::TestLessThan, false),
            CompareOp::Gt => (Bytecode::TestGreaterThan, false),
            CompareOp::Le => (Bytecode::TestLessThanOrEqual, false),
            CompareOp::Ge => (Bytecode::TestGreaterThanOrEqual, false),
        };
        self.out(b, &[i32::from(reg).into(), 0]);
        if negate {
            self.out(Bytecode::LogicalNot, &[]);
        }
    }

    pub fn test_typeof(&mut self, literal: TypeOfLiteral) {
        self.out(Bytecode::TestTypeOf, &[OperandValue::from(literal as u8)]);
    }

    pub fn test_undetectable(&mut self) {
        self.out(Bytecode::TestUndetectable, &[]);
    }

    pub fn test_null(&mut self) {
        self.out(Bytecode::TestNull, &[]);
    }

    pub fn test_undefined(&mut self) {
        self.out(Bytecode::TestUndefined, &[]);
    }

    pub fn test_reference_equal(&mut self, reg: Register) {
        self.out(Bytecode::TestReferenceEqual, &[i32::from(reg).into()]);
    }

    pub fn test_instance_of(&mut self, reg: Register) {
        let s = self.slot(FeedbackSlotKind::InstanceOf);
        self.out(Bytecode::TestInstanceOf, &[i32::from(reg).into(), s as OperandValue]);
    }

    pub fn test_in(&mut self, reg: Register) {
        let s = self.slot(FeedbackSlotKind::HasKeyed);
        self.out(Bytecode::TestIn, &[i32::from(reg).into(), s as OperandValue]);
    }

    /// `Inc` / `Dec` / `Negate` / `BitwiseNot` / `TypeOf` (feedback-taking unary operations)
    pub fn unary(&mut self, b: Bytecode) {
        let kind = if b == Bytecode::TypeOf { FeedbackSlotKind::TypeOf } else { FeedbackSlotKind::BinaryOp };
        let s = self.slot(kind);
        self.out(b, &[s as OperandValue]);
    }

    /// Bytecodes without operands (`LogicalNot`, `ToBooleanLogicalNot`, `ToName`, `ToString`, `Return`, `Throw` …)
    pub fn plain(&mut self, b: Bytecode) {
        self.out(b, &[]);
    }

    pub fn to_numeric(&mut self) {
        let s = self.slot(FeedbackSlotKind::BinaryOp);
        self.out(Bytecode::ToNumeric, &[s as OperandValue]);
    }

    pub fn to_number(&mut self) {
        let s = self.slot(FeedbackSlotKind::BinaryOp);
        self.out(Bytecode::ToNumber, &[s as OperandValue]);
    }

    pub fn to_object(&mut self, out: Register) {
        self.out(Bytecode::ToObject, &[i32::from(out).into()]);
    }

    // ---- jumps ----

    /// A forward jump to `label` (V8's `EmitJump`: the operand is sized by a constant pool reservation)
    pub fn jump(&mut self, b: Bytecode, label: &mut Label) {
        self.jump_with(b, label, &[]);
    }

    /// A forward jump whose target is its first operand, followed by `rest`
    fn jump_with(&mut self, b: Bytecode, label: &mut Label, rest: &[OperandValue]) {
        debug_assert!(label.bound.is_none(), "backward jumps are JumpLoop");
        let entry = self.constants.len();
        self.constants.push(Constant::Reserved);
        // A placeholder that needs the reservation's scale (patched when the label is bound)
        let placeholder: OperandValue = match OperandScale::for_unsigned(u32::try_from(entry).unwrap_or(u32::MAX)) {
            OperandScale::Single => 0,
            OperandScale::Double => 0x100,
            OperandScale::Quadruple => 0x1_0000,
        };
        let mut ops = alloc::vec![placeholder];
        ops.extend_from_slice(rest);
        let at = self.out(b, &ops);
        label.refs.push((at, entry));
        self.unbound += 1;
    }

    /// Binds `label` here, patching the jumps to it (V8's `PatchJump`)
    pub fn bind(&mut self, label: &mut Label) {
        let target = self.offset();
        label.bound = Some(target);
        for (at, entry) in core::mem::take(&mut label.refs) {
            self.patch(at, entry, target);
        }
    }

    fn patch(&mut self, at: usize, entry: usize, target: usize) {
        let code = &mut self.enc.code;
        let (scale, start) = match Bytecode::from_u8(code[at]).and_then(Bytecode::prefix_scale) {
            Some(s) => (s, at + 1),
            None => (OperandScale::Single, at),
        };
        let delta = u32::try_from(target - start).unwrap_or(u32::MAX);
        let width = usize::from(j8bytecode::OperandSize::from(scale));
        let fits = OperandScale::for_unsigned(delta) <= scale;
        let value = if fits {
            // Fits: drop the reservation (the entry stays as a hole V8 would also leave, reused by the next constant)
            delta
        } else {
            let b = Bytecode::from_u8(code[start]).expect("a jump");
            code[start] = Self::with_constant_operand(b) as u8;
            self.constants[entry] = Constant::Offset(delta);
            u32::try_from(entry).unwrap_or(u32::MAX)
        };
        code[start + 1..start + 1 + width].copy_from_slice(&value.to_le_bytes()[..width]);
        if fits {
            self.discard(entry);
        }
        self.unbound -= 1;
    }

    /// Drops a reservation: the last entry is popped, an inner one left `Reserved` (unused)
    fn discard(&mut self, entry: usize) {
        if entry + 1 == self.constants.len() {
            self.constants.pop();
        }
    }

    /// The `*Constant` form of a forward jump (V8's `GetJumpWithConstantOperand`)
    fn with_constant_operand(b: Bytecode) -> Bytecode {
        match b {
            Bytecode::Jump => Bytecode::JumpConstant,
            Bytecode::JumpIfTrue => Bytecode::JumpIfTrueConstant,
            Bytecode::JumpIfFalse => Bytecode::JumpIfFalseConstant,
            Bytecode::JumpIfToBooleanTrue => Bytecode::JumpIfToBooleanTrueConstant,
            Bytecode::JumpIfToBooleanFalse => Bytecode::JumpIfToBooleanFalseConstant,
            Bytecode::JumpIfNull => Bytecode::JumpIfNullConstant,
            Bytecode::JumpIfNotNull => Bytecode::JumpIfNotNullConstant,
            Bytecode::JumpIfUndefined => Bytecode::JumpIfUndefinedConstant,
            Bytecode::JumpIfNotUndefined => Bytecode::JumpIfNotUndefinedConstant,
            Bytecode::JumpIfUndefinedOrNull => Bytecode::JumpIfUndefinedOrNullConstant,
            Bytecode::JumpIfJSReceiver => Bytecode::JumpIfJSReceiverConstant,
            Bytecode::JumpIfForInDone => Bytecode::JumpIfForInDoneConstant,
            other => unreachable!("{other:?} has no constant form"),
        }
    }

    pub fn loop_header(&self) -> LoopHeader {
        LoopHeader(self.offset())
    }

    /// Jumps back to `header` (V8's `JumpLoop`: the offset back, the loop depth, a feedback slot for OSR)
    pub fn jump_loop(&mut self, header: LoopHeader, depth: u32) {
        let s = self.slot(FeedbackSlotKind::JumpLoop);
        // The offset counts from the JumpLoop bytecode itself (its prefix, if any, not counted): compute it for the
        // smallest form first, then once more if the operands need a prefix
        let back = self.offset() - header.0;
        let mut ops = [back as OperandValue, depth.into(), s as OperandValue];
        if Encoder::scale(Bytecode::JumpLoop, &ops) > OperandScale::Single {
            ops[0] = (back + 1) as OperandValue;
        }
        self.out(Bytecode::JumpLoop, &ops);
    }

    /// A jump table over `size` cases from `case_base` (targets bound with [`Self::bind_case`])
    pub fn jump_table(&mut self, size: u32, case_base: i32) -> JumpTable {
        let constant = self.constants.len();
        for _ in 0..size {
            self.constants.push(Constant::Reserved);
        }
        JumpTable { constant, size, case_base, switch_at: 0 }
    }

    pub fn switch_on_smi(&mut self, table: &mut JumpTable) {
        let at = self.out(Bytecode::SwitchOnSmiNoFeedback, &[table.constant as OperandValue, table.size.into(), table.case_base.into()]);
        table.switch_at = at + usize::from(self.enc.code[at] != Bytecode::SwitchOnSmiNoFeedback as u8);
    }

    pub fn switch_on_generator_state(&mut self, generator: Register, table: &mut JumpTable) {
        let at = self.out(Bytecode::SwitchOnGeneratorState, &[i32::from(generator).into(), table.constant as OperandValue, table.size.into()]);
        table.switch_at = at + usize::from(self.enc.code[at] != Bytecode::SwitchOnGeneratorState as u8);
    }

    /// Binds case `case` of `table` here: its constant holds the offset relative to the switch bytecode
    pub fn bind_case(&mut self, table: &JumpTable, case: i32) {
        let i = usize::try_from(case - table.case_base).expect("case in range");
        self.constants[table.constant + i] = Constant::Offset(u32::try_from(self.offset() - table.switch_at).unwrap_or(u32::MAX));
    }

    // ---- calls ----

    /// `callable(receiver, …args)` with the receiver as `args[0]` (V8's `CallProperty`)
    pub fn call_property(&mut self, callable: Register, args: RegisterList) {
        let s = self.slot(FeedbackSlotKind::Call);
        let c = i32::from(callable).into();
        let a = |i: u32| OperandValue::from(i32::from(args.at(i)));
        match args.count {
            1 => self.out(Bytecode::CallProperty0, &[c, a(0), s as OperandValue]),
            2 => self.out(Bytecode::CallProperty1, &[c, a(0), a(1), s as OperandValue]),
            3 => self.out(Bytecode::CallProperty2, &[c, a(0), a(1), a(2), s as OperandValue]),
            n => self.out(Bytecode::CallProperty, &[c, a(0), n.into(), s as OperandValue]),
        };
    }

    /// `callable(…args)` with an undefined receiver (V8's `CallUndefinedReceiver`)
    pub fn call_undefined_receiver(&mut self, callable: Register, args: RegisterList) {
        let s = self.slot(FeedbackSlotKind::Call);
        let c = i32::from(callable).into();
        let a = |i: u32| OperandValue::from(i32::from(args.at(i)));
        match args.count {
            0 => self.out(Bytecode::CallUndefinedReceiver0, &[c, s as OperandValue]),
            1 => self.out(Bytecode::CallUndefinedReceiver1, &[c, a(0), s as OperandValue]),
            2 => self.out(Bytecode::CallUndefinedReceiver2, &[c, a(0), a(1), s as OperandValue]),
            n => self.out(Bytecode::CallUndefinedReceiver, &[c, a(0), n.into(), s as OperandValue]),
        };
    }

    /// `callable` with receiver `args[0]` that may be anything (V8's `CallAnyReceiver`)
    pub fn call_any_receiver(&mut self, callable: Register, args: RegisterList) {
        let s = self.slot(FeedbackSlotKind::Call);
        self.out(Bytecode::CallAnyReceiver, &[i32::from(callable).into(), i32::from(args.first).into(), args.count.into(), s as OperandValue]);
    }

    /// The last argument is spread (V8's `CallWithSpread`)
    pub fn call_with_spread(&mut self, callable: Register, args: RegisterList) {
        let s = self.slot(FeedbackSlotKind::Call);
        self.out(Bytecode::CallWithSpread, &[i32::from(callable).into(), i32::from(args.first).into(), args.count.into(), s as OperandValue]);
    }

    /// `new constructor(…args)` with `new.target` in the accumulator (V8's `Construct`)
    pub fn construct(&mut self, constructor: Register, args: RegisterList) {
        let s = self.slot(FeedbackSlotKind::Call);
        self.out(Bytecode::Construct, &[i32::from(constructor).into(), i32::from(args.first).into(), args.count.into(), s as OperandValue]);
    }

    pub fn construct_with_spread(&mut self, constructor: Register, args: RegisterList) {
        let s = self.slot(FeedbackSlotKind::Call);
        self.out(Bytecode::ConstructWithSpread, &[i32::from(constructor).into(), i32::from(args.first).into(), args.count.into(), s as OperandValue]);
    }

    /// A runtime function (V8's `CallRuntime`: `InvokeIntrinsic` when it is one)
    pub fn call_runtime(&mut self, f: Runtime, args: RegisterList) {
        match Self::intrinsic(f) {
            Some(i) => self.out(Bytecode::InvokeIntrinsic, &[OperandValue::from(i as u8), i32::from(args.first).into(), args.count.into()]),
            None => self.out(Bytecode::CallRuntime, &[OperandValue::from(f as u16), i32::from(args.first).into(), args.count.into()]),
        };
    }

    /// The intrinsic standing for runtime function `f` (V8's `IntrinsicsHelper::FromRuntimeId`: `InlineX` ⇒ `X`)
    fn intrinsic(f: Runtime) -> Option<Intrinsic> {
        let name = alloc::format!("{f:?}");
        let inner = name.strip_prefix("Inline")?;
        Intrinsic::ALL.into_iter().find(|i| alloc::format!("{i:?}") == inner)
    }

    // ---- closures, literals ----

    pub fn create_closure(&mut self, function: usize, flags: u8) {
        let s = self.slot(FeedbackSlotKind::Literal);
        self.out(Bytecode::CreateClosure, &[function as OperandValue, s as OperandValue, flags.into()]);
    }

    pub fn create_object_literal(&mut self, boilerplate: usize, flags: u8) {
        let s = self.slot(FeedbackSlotKind::Literal);
        self.out(Bytecode::CreateObjectLiteral, &[boilerplate as OperandValue, s as OperandValue, flags.into()]);
    }

    pub fn create_array_literal(&mut self, boilerplate: usize, flags: u8) {
        let s = self.slot(FeedbackSlotKind::Literal);
        self.out(Bytecode::CreateArrayLiteral, &[boilerplate as OperandValue, s as OperandValue, flags.into()]);
    }

    pub fn create_empty_array_literal(&mut self) {
        let s = self.slot(FeedbackSlotKind::Literal);
        self.out(Bytecode::CreateEmptyArrayLiteral, &[s as OperandValue]);
    }

    pub fn create_reg_exp_literal(&mut self, pattern: usize, flags: u16) {
        let s = self.slot(FeedbackSlotKind::Literal);
        self.out(Bytecode::CreateRegExpLiteral, &[pattern as OperandValue, s as OperandValue, flags.into()]);
    }

    pub fn get_template_object(&mut self, template: usize) {
        let s = self.slot(FeedbackSlotKind::Literal);
        self.out(Bytecode::GetTemplateObject, &[template as OperandValue, s as OperandValue]);
    }

    pub fn clone_object(&mut self, source: Register, flags: u8) {
        let s = self.slot(FeedbackSlotKind::CloneObject);
        self.out(Bytecode::CloneObject, &[i32::from(source).into(), flags.into(), s as OperandValue]);
    }

    // ---- iteration ----

    pub fn get_iterator(&mut self, object: Register) {
        let load = self.slot(FeedbackSlotKind::LoadProperty);
        let call = self.slot(FeedbackSlotKind::Call);
        self.out(Bytecode::GetIterator, &[i32::from(object).into(), load as OperandValue, call as OperandValue]);
    }

    pub fn for_in_enumerate(&mut self, receiver: Register) {
        self.out(Bytecode::ForInEnumerate, &[i32::from(receiver).into()]);
    }

    /// Fills three registers from `out` (cache type, array, length); returns the feedback slot shared with `ForInNext`
    pub fn for_in_prepare(&mut self, out: RegisterList) -> usize {
        let s = self.slot(FeedbackSlotKind::ForIn);
        self.out(Bytecode::ForInPrepare, &[i32::from(out.first).into(), s as OperandValue]);
        s
    }

    pub fn jump_if_for_in_done(&mut self, label: &mut Label, index: Register, cache_length: Register) {
        self.jump_with(Bytecode::JumpIfForInDone, label, &[i32::from(index).into(), i32::from(cache_length).into()]);
    }

    pub fn for_in_next(&mut self, receiver: Register, index: Register, cache: RegisterList, slot: usize) {
        self.out(Bytecode::ForInNext, &[i32::from(receiver).into(), i32::from(index).into(), i32::from(cache.first).into(), slot as OperandValue]);
    }

    pub fn for_in_step(&mut self, index: Register) {
        self.out(Bytecode::ForInStep, &[i32::from(index).into()]);
    }

    // ---- generators ----

    pub fn suspend_generator(&mut self, generator: Register, registers: RegisterList, suspend_id: u32) {
        self.out(Bytecode::SuspendGenerator, &[i32::from(generator).into(), i32::from(registers.first).into(), registers.count.into(), suspend_id.into()]);
    }

    pub fn resume_generator(&mut self, generator: Register, registers: RegisterList) {
        self.out(Bytecode::ResumeGenerator, &[i32::from(generator).into(), i32::from(registers.first).into(), registers.count.into()]);
    }

    // ---- exceptions ----

    /// A new handler table entry (V8's `HandlerTableBuilder::NewHandlerEntry`)
    pub fn new_handler(&mut self, context: Register, prediction: CatchPrediction) -> usize {
        self.handlers.push(HandlerEntry { start: 0, end: 0, handler: 0, context: i32::from(context), prediction });
        self.handlers.len() - 1
    }

    pub fn try_start(&mut self, handler: usize) {
        self.handlers[handler].start = u32::try_from(self.offset()).unwrap_or(u32::MAX);
    }

    pub fn try_end(&mut self, handler: usize) {
        self.handlers[handler].end = u32::try_from(self.offset()).unwrap_or(u32::MAX);
    }

    pub fn handler_target(&mut self, handler: usize) {
        self.handlers[handler].handler = u32::try_from(self.offset()).unwrap_or(u32::MAX);
    }

    pub fn throw_reference_error_if_hole(&mut self, name: &str) {
        let k = self.name(name);
        self.out(Bytecode::ThrowReferenceErrorIfHole, &[k as OperandValue]);
    }

    pub fn create_rest_parameter(&mut self) {
        self.out(Bytecode::CreateRestParameter, &[]);
    }

    pub fn create_arguments(&mut self, mapped: bool) {
        self.out(if mapped { Bytecode::CreateMappedArguments } else { Bytecode::CreateUnmappedArguments }, &[]);
    }

    /// Any bytecode with the given operands (for the rare ones without a helper)
    pub fn raw(&mut self, b: Bytecode, ops: &[OperandValue]) -> usize {
        self.out(b, ops)
    }
}
