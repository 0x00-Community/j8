//! Threaded code: every decoded instruction carries the function that runs it ([`Handler`]). A handler gets the hot
//! state in registers (the instruction, the frame's r0, the accumulator) and ends by jumping straight to the next
//! instruction's handler: with a nightly compiler a guaranteed tail call (`become`), otherwise it returns to the loop in
//! [`Executor::run`], which calls the next one.
//!
//! Hot bytecodes have handlers of their own here; the rest (and the hot ones off their fast path) go through [`slow`],
//! which syncs the state and runs [`Executor::step`]. A handler leaves the chain when the frame changes (calls and
//! returns), on an exception, or when the execution ends.
use j8bytecode::Bytecode;
use j8framework::{Agent, Env, Error, ErrorKind, Value, ValueKind};

use crate::{Args, Budget, Executor, Flow, Frame, Insn, Loaded, Outcome, State, STACK_LIMIT};

/// What the threaded code shares besides the hot state.
#[expect(private_interfaces, reason = "crate-internal types; the module is private")]
pub struct Cx<'r, 'gc, 'x, 'b> {
    pub exe: &'r Executor,
    pub a: &'r mut Agent<'gc, 'x>,
    pub st: &'r mut State<'gc>,
    pub loaded: &'r Loaded,
    pub budget: &'r mut Budget<'b>,
    /// Call depth of the frames below the current execution
    pub depth: usize,
}

/// How a chain of handlers ends.
#[expect(private_interfaces, reason = "crate-internal types; the module is private")]
pub enum Exit<'gc> {
    /// The frame changed (the state is synced): fetch the new frame's function and go on
    Frame,
    Done(Outcome<'gc>),
    Throw(Error),
    /// Without tail calls: the next instruction to run, and the accumulator
    #[cfg(not(tail_calls))]
    Next(*const Insn, Value<'gc>),
}

/// Runs one instruction (see the module docs).
pub type Handler = for<'r, 'gc, 'x, 'b> fn(&mut Cx<'r, 'gc, 'x, 'b>, *const Insn, *mut Value<'gc>, Value<'gc>) -> Exit<'gc>;

/// Goes on with instruction `$pc` (a guaranteed tail call into its handler)
#[cfg(tail_calls)]
macro_rules! next {
    ($cx:ident, $pc:expr, $regs:ident, $acc:expr) => {{
        let pc: *const Insn = $pc;
        // SAFETY: `pc` points into the function's instructions (they end with a sentinel, and jumps land inside)
        become (unsafe { (*pc).handler })($cx, pc, $regs, $acc)
    }};
}

/// Goes on with instruction `$pc` (back to the loop in [`Executor::threaded`])
#[cfg(not(tail_calls))]
macro_rules! next {
    ($cx:ident, $pc:expr, $regs:ident, $acc:expr) => {{
        let pc: *const Insn = $pc;
        return Exit::Next(pc, $acc);
    }};
}

/// Defines handlers: `name(cx, pc, regs, acc, operands) { body }`; the body ends by a `next!` / `fall!` / return.
macro_rules! handlers {
    ($($name:ident($cx:ident, $pc:ident, $regs:ident, $acc:ident, $o:ident) $body:block)*) => {$(
        #[allow(unused_mut, unused_variables, clippy::cast_precision_loss, clippy::cast_possible_truncation, reason = "operands are in range")]
        fn $name<'r, 'gc, 'x, 'b>($cx: &mut Cx<'r, 'gc, 'x, 'b>, $pc: *const Insn, $regs: *mut Value<'gc>, mut $acc: Value<'gc>) -> Exit<'gc> {
            // SAFETY: `pc` points at a decoded instruction of the running function
            let $o = unsafe { &(*$pc).operands };
            $body
        }
    )*};
}

/// The next instruction
macro_rules! step {
    ($pc:ident) => {
        // SAFETY: an instruction that falls through is never the last (the sentinel follows)
        unsafe { $pc.add(1) }
    };
}

/// A jump to the instruction's target
macro_rules! target {
    ($cx:ident, $pc:ident) => {
        // SAFETY: decoding made every jump target an index into the instructions
        unsafe { $cx.loaded.insns.as_ptr().add((*$pc).target as usize) }
    };
}

/// Register `k` (a decoded operand); a fixed frame slot through the frame
macro_rules! reg {
    ($cx:ident, $regs:ident, $k:expr) => {{
        let k = $k;
        if k > Loaded::SPECIAL_LIMIT {
            // SAFETY: decoded register operands index the running frame's part of the register file
            unsafe { *$regs.offset(k as isize) }
        } else {
            Executor::reg(&$cx.st.regs, $cx.st.f(), k)
        }
    }};
}

/// Hands the instruction to the slow path
#[cfg(tail_calls)]
macro_rules! fall {
    ($cx:ident, $pc:ident, $regs:ident, $acc:ident) => {
        become slow($cx, $pc, $regs, $acc)
    };
}

#[cfg(not(tail_calls))]
macro_rules! fall {
    ($cx:ident, $pc:ident, $regs:ident, $acc:ident) => {
        return slow($cx, $pc, $regs, $acc)
    };
}

/// Two numbers inline, anything else on the slow path
macro_rules! arith {
    ($cx:ident, $pc:ident, $regs:ident, $acc:ident, $l:expr, $r:expr, $sym:tt) => {{
        let (l, r) = ($l, $r);
        let (Some(x), Some(y)) = (l.as_number(), r.as_number()) else { fall!($cx, $pc, $regs, $acc) };
        next!($cx, step!($pc), $regs, Value::Number(x $sym y))
    }};
}

macro_rules! relational {
    ($cx:ident, $pc:ident, $regs:ident, $acc:ident, $o:ident, $sym:tt) => {{
        let l = reg!($cx, $regs, $o[0]);
        let (Some(x), Some(y)) = (l.as_number(), $acc.as_number()) else { fall!($cx, $pc, $regs, $acc) };
        next!($cx, step!($pc), $regs, Value::Bool(x $sym y))
    }};
}

handlers! {
    ldar(cx, pc, regs, acc, o) { next!(cx, step!(pc), regs, reg!(cx, regs, o[0])) }
    star(cx, pc, regs, acc, o) {
        if o[0] > Loaded::SPECIAL_LIMIT {
            // SAFETY: as in `reg!`
            unsafe { *regs.offset(o[0] as isize) = acc };
            next!(cx, step!(pc), regs, acc)
        }
        fall!(cx, pc, regs, acc)
    }
    mov(cx, pc, regs, acc, o) {
        if o[1] > Loaded::SPECIAL_LIMIT {
            let v = reg!(cx, regs, o[0]);
            // SAFETY: as in `reg!`
            unsafe { *regs.offset(o[1] as isize) = v };
            next!(cx, step!(pc), regs, acc)
        }
        fall!(cx, pc, regs, acc)
    }
    lda_zero(cx, pc, regs, acc, o) { next!(cx, step!(pc), regs, Value::Number(0.0)) }
    lda_smi(cx, pc, regs, acc, o) { next!(cx, step!(pc), regs, Value::Number(o[0] as f64)) }
    lda_undefined(cx, pc, regs, acc, o) { next!(cx, step!(pc), regs, Value::Undefined) }
    lda_null(cx, pc, regs, acc, o) { next!(cx, step!(pc), regs, Value::Null) }
    lda_the_hole(cx, pc, regs, acc, o) { next!(cx, step!(pc), regs, Value::Uninit) }
    lda_true(cx, pc, regs, acc, o) { next!(cx, step!(pc), regs, Value::Bool(true)) }
    lda_false(cx, pc, regs, acc, o) { next!(cx, step!(pc), regs, Value::Bool(false)) }
    add(cx, pc, regs, acc, o) { arith!(cx, pc, regs, acc, reg!(cx, regs, o[0]), acc, +) }
    sub(cx, pc, regs, acc, o) { arith!(cx, pc, regs, acc, reg!(cx, regs, o[0]), acc, -) }
    mul(cx, pc, regs, acc, o) { arith!(cx, pc, regs, acc, reg!(cx, regs, o[0]), acc, *) }
    div(cx, pc, regs, acc, o) { arith!(cx, pc, regs, acc, reg!(cx, regs, o[0]), acc, /) }
    add_smi(cx, pc, regs, acc, o) { arith!(cx, pc, regs, acc, acc, Value::Number(o[0] as f64), +) }
    sub_smi(cx, pc, regs, acc, o) { arith!(cx, pc, regs, acc, acc, Value::Number(o[0] as f64), -) }
    mul_smi(cx, pc, regs, acc, o) { arith!(cx, pc, regs, acc, acc, Value::Number(o[0] as f64), *) }
    div_smi(cx, pc, regs, acc, o) { arith!(cx, pc, regs, acc, acc, Value::Number(o[0] as f64), /) }
    inc(cx, pc, regs, acc, o) {
        if let Some(x) = acc.as_number() {
            next!(cx, step!(pc), regs, Value::Number(x + 1.0))
        }
        fall!(cx, pc, regs, acc)
    }
    dec(cx, pc, regs, acc, o) {
        if let Some(x) = acc.as_number() {
            next!(cx, step!(pc), regs, Value::Number(x - 1.0))
        }
        fall!(cx, pc, regs, acc)
    }
    test_less_than(cx, pc, regs, acc, o) { relational!(cx, pc, regs, acc, o, <) }
    test_greater_than(cx, pc, regs, acc, o) { relational!(cx, pc, regs, acc, o, >) }
    test_less_than_or_equal(cx, pc, regs, acc, o) { relational!(cx, pc, regs, acc, o, <=) }
    test_greater_than_or_equal(cx, pc, regs, acc, o) { relational!(cx, pc, regs, acc, o, >=) }
    test_equal_strict(cx, pc, regs, acc, o) {
        let l = reg!(cx, regs, o[0]);
        match (l.as_number(), acc.as_number()) {
            (Some(x), Some(y)) => next!(cx, step!(pc), regs, Value::Bool(x == y)),
            _ => fall!(cx, pc, regs, acc),
        }
    }
    jump(cx, pc, regs, acc, o) { next!(cx, target!(cx, pc), regs, acc) }
    jump_if_true(cx, pc, regs, acc, o) {
        if matches!(acc.kind(), ValueKind::Bool(true)) {
            next!(cx, target!(cx, pc), regs, acc)
        }
        next!(cx, step!(pc), regs, acc)
    }
    jump_if_false(cx, pc, regs, acc, o) {
        if matches!(acc.kind(), ValueKind::Bool(false)) {
            next!(cx, target!(cx, pc), regs, acc)
        }
        next!(cx, step!(pc), regs, acc)
    }
    jump_if_to_boolean_true(cx, pc, regs, acc, o) {
        if cx.a.to_boolean(acc) {
            next!(cx, target!(cx, pc), regs, acc)
        }
        next!(cx, step!(pc), regs, acc)
    }
    jump_if_to_boolean_false(cx, pc, regs, acc, o) {
        if cx.a.to_boolean(acc) {
            next!(cx, step!(pc), regs, acc)
        }
        next!(cx, target!(cx, pc), regs, acc)
    }
    jump_loop(cx, pc, regs, acc, o) {
        // The budget counts back edges; when this run of steps is spent, the slow path refills or pauses
        if cx.budget.tick > 0 {
            cx.budget.tick -= 1;
            next!(cx, target!(cx, pc), regs, acc)
        }
        fall!(cx, pc, regs, acc)
    }
    lda_current_context_slot(cx, pc, regs, acc, o) {
        // SAFETY: the header's context slot is right below r0
        match unsafe { *regs.sub(Frame::CONTEXT) }.as_context() {
            Some(e) => next!(cx, step!(pc), regs, e.load(Executor::env_index(o[0]))),
            None => fall!(cx, pc, regs, acc),
        }
    }
    sta_current_context_slot(cx, pc, regs, acc, o) {
        // SAFETY: as above
        match unsafe { *regs.sub(Frame::CONTEXT) }.as_context() {
            Some(e) => {
                Env::set(e, cx.a.mc, Executor::env_index(o[0]), acc);
                next!(cx, step!(pc), regs, acc)
            }
            None => fall!(cx, pc, regs, acc),
        }
    }
    throw_reference_error_if_hole(cx, pc, regs, acc, o) {
        if matches!(acc.kind(), ValueKind::Uninit) {
            fall!(cx, pc, regs, acc)
        }
        next!(cx, step!(pc), regs, acc)
    }
    get_named_property(cx, pc, regs, acc, o) {
        if let ValueKind::Object(ob) = reg!(cx, regs, o[0]).kind()
            && let Some(v) = cx.loaded.ics[o[2] as usize].get(ob)
        {
            next!(cx, step!(pc), regs, v)
        }
        fall!(cx, pc, regs, acc)
    }
    lda_global(cx, pc, regs, acc, o) {
        let (scope, global) = (cx.a.realm.script_scope, cx.a.realm.global);
        match cx.loaded.globals[o[1] as usize].get(scope, global) {
            Some(v) => next!(cx, step!(pc), regs, v),
            None => fall!(cx, pc, regs, acc),
        }
    }
}

/// A call of an ordinary JS function (not a class constructor, generator or async function) from the running frame:
/// the callee's frame record goes on the frame stack and its code starts right here, without leaving the threaded
/// code (V8's `InterpreterEntryTrampoline`). `None`: anything else, for the slow path.
#[inline]
fn enter<'r, 'gc>(cx: &mut Cx<'r, 'gc, '_, '_>, pc: *const Insn, callee: Value<'gc>, this: Value<'gc>, args: Args<'_, 'gc>) -> Option<(*const Insn, *mut Value<'gc>)> {
    let ValueKind::Object(o) = callee.kind() else { return None };
    let c = o.closure()?;
    let l: &'r Loaded = cx.exe.function(c.func);
    if l.class_constructor || l.generator || l.asynchronous || cx.depth + cx.st.frames.len() + 1 >= STACK_LIMIT {
        return None;
    }
    // SAFETY: `pc` points into the running function's instructions
    let at = usize::try_from(unsafe { pc.offset_from(cx.loaded.insns.as_ptr()) }).unwrap_or(0);
    let f = cx.st.f_mut();
    f.insn = at;
    f.pc = at + 1;
    let next = cx.exe.open_with(cx.a, &mut cx.st.regs, l, c, callee, this, args);
    let base = next.base;
    cx.st.frames.push(next);
    cx.loaded = l;
    Some((l.insns.as_ptr(), cx.st.regs.as_mut_ptr().wrapping_add(base)))
}

/// A call with the callee in register `o[0]`, `this` and up to two arguments at hand
macro_rules! call {
    ($cx:ident, $pc:ident, $regs:ident, $acc:ident, $o:ident, $this:expr, [$($arg:expr),*]) => {{
        let callee = reg!($cx, $regs, $o[0]);
        let args = [$($arg),*];
        match enter($cx, $pc, callee, $this, Args::Values(&args)) {
            Some((pc, regs)) => next!($cx, pc, regs, Value::Undefined),
            None => fall!($cx, $pc, $regs, $acc),
        }
    }};
}

handlers! {
    call_undefined_receiver0(cx, pc, regs, acc, o) { call!(cx, pc, regs, acc, o, Value::Undefined, []) }
    call_undefined_receiver1(cx, pc, regs, acc, o) { call!(cx, pc, regs, acc, o, Value::Undefined, [reg!(cx, regs, o[1])]) }
    call_undefined_receiver2(cx, pc, regs, acc, o) { call!(cx, pc, regs, acc, o, Value::Undefined, [reg!(cx, regs, o[1]), reg!(cx, regs, o[2])]) }
    call_property0(cx, pc, regs, acc, o) { call!(cx, pc, regs, acc, o, reg!(cx, regs, o[1]), []) }
    call_property1(cx, pc, regs, acc, o) { call!(cx, pc, regs, acc, o, reg!(cx, regs, o[1]), [reg!(cx, regs, o[2])]) }
    call_property2(cx, pc, regs, acc, o) { call!(cx, pc, regs, acc, o, reg!(cx, regs, o[1]), [reg!(cx, regs, o[2]), reg!(cx, regs, o[3])]) }
    // A plain call's frame returns to its caller's next instruction (construct frames, and the outermost frame, take
    // the slow path)
    return_(cx, pc, regs, acc, o) {
        let st = &mut *cx.st;
        // SAFETY: the header's new.target slot is below r0
        if st.frames.len() < 2 || unsafe { *regs.sub(Frame::NEW_TARGET) } != Value::Undefined {
            fall!(cx, pc, regs, acc)
        }
        st.pop_frame();
        let caller = st.f();
        let (func, at, base) = (caller.func, caller.pc, caller.base);
        let l = cx.exe.function(func);
        cx.loaded = l;
        // SAFETY: the caller's `pc` was synced when it called (an instruction after the call)
        let pc = unsafe { l.insns.as_ptr().add(at) };
        let regs = cx.st.regs.as_mut_ptr().wrapping_add(base);
        next!(cx, pc, regs, acc)
    }
}

/// Syncs the state and runs the instruction through [`Executor::step`]
#[inline(never)]
fn slow<'r, 'gc, 'x, 'b>(cx: &mut Cx<'r, 'gc, 'x, 'b>, pc: *const Insn, _: *mut Value<'gc>, acc: Value<'gc>) -> Exit<'gc> {
    let base = cx.loaded.insns.as_ptr();
    // SAFETY: `pc` points into the same instructions
    let at = usize::try_from(unsafe { pc.offset_from(base) }).unwrap_or(0);
    let st = &mut *cx.st;
    let f = st.f_mut();
    f.insn = at;
    f.pc = at + 1;
    st.acc = acc;
    let level = cx.depth + st.frames.len();
    // SAFETY: as above
    let d = unsafe { &*pc };
    match cx.exe.step(cx.a, st, cx.loaded, d, level, cx.budget) {
        Ok(Flow::Next) => {
            // Calls to host functions don't touch this frame's registers, but read them back anyway
            #[cfg_attr(not(tail_calls), expect(unused_variables, reason = "the loop reloads the registers"))]
            let regs = st.regs.as_mut_ptr().wrapping_add(st.f().base);
            let acc = st.acc;
            // SAFETY: `step` leaves `pc` inside the instructions
            next!(cx, unsafe { base.add(cx.st.f().pc) }, regs, acc)
        }
        Ok(Flow::Frame) => Exit::Frame,
        Ok(Flow::Done(o)) => Exit::Done(o),
        Err(e) => Exit::Throw(e),
    }
}

/// The sentinel after the last instruction (never reached by valid code)
fn illegal<'r, 'gc, 'x, 'b>(cx: &mut Cx<'r, 'gc, 'x, 'b>, _: *const Insn, _: *mut Value<'gc>, _: Value<'gc>) -> Exit<'gc> {
    Exit::Throw(cx.a.throw(ErrorKind::Syntax, "ran past the end of the bytecode"))
}

impl Insn {
    /// The handler for `bytecode`
    pub fn handler(bytecode: Bytecode) -> Handler {
        use Bytecode as B;
        match bytecode {
            B::Ldar => ldar,
            B::Star => star,
            B::Mov => mov,
            B::LdaZero => lda_zero,
            B::LdaSmi => lda_smi,
            B::LdaUndefined => lda_undefined,
            B::LdaNull => lda_null,
            B::LdaTheHole => lda_the_hole,
            B::LdaTrue => lda_true,
            B::LdaFalse => lda_false,
            B::Add => add,
            B::Sub => sub,
            B::Mul => mul,
            B::Div => div,
            B::AddSmi => add_smi,
            B::SubSmi => sub_smi,
            B::MulSmi => mul_smi,
            B::DivSmi => div_smi,
            B::Inc => inc,
            B::Dec => dec,
            B::TestLessThan => test_less_than,
            B::TestGreaterThan => test_greater_than,
            B::TestLessThanOrEqual => test_less_than_or_equal,
            B::TestGreaterThanOrEqual => test_greater_than_or_equal,
            B::TestEqualStrict => test_equal_strict,
            B::Jump | B::JumpConstant => jump,
            B::JumpIfTrue | B::JumpIfTrueConstant => jump_if_true,
            B::JumpIfFalse | B::JumpIfFalseConstant => jump_if_false,
            B::JumpIfToBooleanTrue | B::JumpIfToBooleanTrueConstant => jump_if_to_boolean_true,
            B::JumpIfToBooleanFalse | B::JumpIfToBooleanFalseConstant => jump_if_to_boolean_false,
            B::JumpLoop => jump_loop,
            B::LdaCurrentContextSlot | B::LdaCurrentContextSlotNoCell | B::LdaImmutableCurrentContextSlot => lda_current_context_slot,
            B::StaCurrentContextSlot | B::StaCurrentContextSlotNoCell => sta_current_context_slot,
            B::ThrowReferenceErrorIfHole => throw_reference_error_if_hole,
            B::GetNamedProperty => get_named_property,
            B::LdaGlobal => lda_global,
            B::CallUndefinedReceiver0 => call_undefined_receiver0,
            B::CallUndefinedReceiver1 => call_undefined_receiver1,
            B::CallUndefinedReceiver2 => call_undefined_receiver2,
            B::CallProperty0 => call_property0,
            B::CallProperty1 => call_property1,
            B::CallProperty2 => call_property2,
            B::Return => return_,
            B::Illegal => illegal,
            _ => slow,
        }
    }
}

impl Executor {
    /// Runs the current frame's instructions until the frame changes or the execution ends.
    pub fn threaded<'gc>(cx: &mut Cx<'_, 'gc, '_, '_>) -> Exit<'gc> {
        let st = &mut *cx.st;
        let pc = &raw const cx.loaded.insns[st.f().pc];
        let regs = st.regs.as_mut_ptr().wrapping_add(st.f().base);
        let acc = st.acc;
        // SAFETY: `pc` points at an instruction of the running function
        let h = unsafe { (*pc).handler };
        #[cfg(tail_calls)]
        {
            h(cx, pc, regs, acc)
        }
        #[cfg(not(tail_calls))]
        {
            let (mut h, mut pc, mut acc, mut regs) = (h, pc, acc, regs);
            loop {
                match h(cx, pc, regs, acc) {
                    Exit::Next(p, a) => {
                        pc = p;
                        acc = a;
                        regs = cx.st.regs.as_mut_ptr().wrapping_add(cx.st.f().base);
                        // SAFETY: as above
                        h = unsafe { (*pc).handler };
                    }
                    e => return e,
                }
            }
        }
    }
}
