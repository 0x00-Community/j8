//! j8jit: the JIT for j8 (needs std; currently x86-64 Unix only).
//!
//! Implements the executor's [`Tier`]: every time an ordinary JS function enters a frame, it comes here first:
//!
//! ```text
//! calls / loop iterations below threshold        ⇒ count once, interpret as usual
//! threshold reached                              ⇒ j8compiler compiles it to machine code (on failure, remember it
//!                                                  and never retry)
//! on frame entry: whole function in machine code ⇒ run directly on this frame's registers and accumulator
//! on a loop back-jump: this loop header exists   ⇒ switch into machine code at the loop header (OSR)
//!   returned                                     ⇒ result in the accumulator, no frame needed (OSR stops before return,
//!                                                  and the interpreter finishes)
//!   stopped before some instruction              ⇒ a fast-path precondition failed or machine code doesn't accept the
//!                                                  instruction; the interpreter continues from there
//! ```
mod code;
mod helpers;

use core::cell::RefCell;

use j8virtasm::Function;
use j8compiler::Compiler;
use j8executor::{Tier, Tiered};
use j8framework::{FuncId, Pool, Task, Value};

use code::Code;

/// A function's state in the JIT.
enum Slot {
    /// Not hot yet: how many times entered, how many loop iterations
    Cold { calls: u32, loops: u32 },
    /// Almost hot (past half the threshold): keep a copy of the bytecode, to be compiled along when another function
    /// gets hot
    Warm { calls: u32, loops: u32, func: Function },
    Ready(Ready),
    /// Cannot be compiled (rejected by the backend); never retried
    Failed,
}

/// After recording hotness.
enum Heat<'s> {
    Cold,
    Ready(&'s Ready),
    Never,
}

/// A compiled function.
struct Ready {
    code: Code,
    /// The whole function is in machine code: can run directly on frame entry (otherwise only switched into at loop
    /// headers)
    pure: bool,
    /// Loop headers that can be entered midway
    entries: Vec<u32>,
}

/// ⭐ The JIT: handed to the executor (`Executor::with_tier`).
pub struct Jit {
    /// How many calls count as hot
    threshold: u32,
    /// By function id
    slots: RefCell<Vec<Slot>>,
    /// Created only when a function first needs compiling
    compiler: core::cell::OnceCell<Compiler>,
}

impl Default for Jit {
    fn default() -> Self {
        Self::from(Jit::THRESHOLD)
    }
}

impl From<u32> for Jit {
    /// Compiles after `threshold` calls (or [`LOOP_FACTOR`](Jit::LOOP_FACTOR) times as many loop iterations) (0:
    /// compile on the first one).
    fn from(threshold: u32) -> Self {
        Self { threshold, slots: RefCell::new(Vec::new()), compiler: core::cell::OnceCell::new() }
    }
}

impl Jit {
    /// Default hotness threshold (call count)
    pub const THRESHOLD: u32 = 100;
    /// The loop iteration threshold is this many times the call threshold (an iteration is lighter than a call)
    pub const LOOP_FACTOR: u32 = 10;
    /// Past this fraction (the denominator) of the threshold counts as almost hot
    const WARM_DIV: u32 = 2;

    /// Number of functions compiled and number that could not be compiled (for diagnostics).
    pub fn counts(&self) -> (usize, usize) {
        let slots = self.slots.borrow();
        let ready = slots.iter().filter(|s| matches!(s, Slot::Ready(_))).count();
        let failed = slots.iter().filter(|s| matches!(s, Slot::Failed)).count();
        (ready, failed)
    }

    /// Installs compiled machine code in executable memory: on success it is `Ready`, otherwise recorded as
    /// uncompilable.
    fn install(native: Result<j8compiler::Native, j8compiler::CompileError>) -> Slot {
        match native.ok().and_then(|n| Code::load(&n).map(|code| (code, n))) {
            Some((code, native)) => Slot::Ready(Ready { code, pure: native.pure(), entries: native.entries }),
            None => Slot::Failed,
        }
    }

    /// Compiles `func`, along with the hottest few almost-hot functions, in parallel on the thread pool (the compiler
    /// is shared across threads).
    fn compile_batch(&self, pool: &dyn Pool, slots: &mut [Slot], at: usize, func: &Function) {
        let mut warm: Vec<(usize, u64)> = slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match s {
                Slot::Warm { calls, loops, .. } if i != at => Some((i, u64::from(*calls) * u64::from(Self::LOOP_FACTOR) + u64::from(*loops))),
                _ => None,
            })
            .collect();
        warm.sort_unstable_by_key(|&(_, heat)| core::cmp::Reverse(heat));
        warm.truncate(pool.threads().saturating_sub(1));
        // Take out the almost-hot bytecode (replaced with the result after compiling)
        let others: Vec<(usize, Function)> = warm
            .iter()
            .filter_map(|&(i, _)| match core::mem::replace(&mut slots[i], Slot::Failed) {
                Slot::Warm { func, .. } => Some((i, func)),
                s => {
                    slots[i] = s;
                    None
                }
            })
            .collect();
        let compiler = self.compiler.get_or_init(Compiler::default);
        let funcs: Vec<&Function> = core::iter::once(func).chain(others.iter().map(|(_, f)| f)).collect();
        let mut out: Vec<Option<Result<j8compiler::Native, j8compiler::CompileError>>> = funcs.iter().map(|_| None).collect();
        let jobs: Vec<Task<'_>> = out.iter_mut().zip(&funcs).map(|(o, f)| Box::new(move || *o = Some(compiler.compile(f))) as Task<'_>).collect();
        pool.run(jobs);
        let targets = core::iter::once(at).chain(others.iter().map(|&(i, _)| i));
        for (i, native) in targets.zip(out) {
            slots[i] = native.map_or(Slot::Failed, Self::install);
        }
    }

    /// Records one unit of hotness (`loop_turn`: one loop iteration, otherwise one call): when almost hot, keeps the
    /// bytecode; when hot enough, compiles (together with other almost-hot functions).
    fn heat<'s>(&self, pool: &dyn Pool, slots: &'s mut Vec<Slot>, id: FuncId, func: &Function, loop_turn: bool) -> Heat<'s> {
        let at = id.0 as usize;
        if at >= slots.len() {
            slots.resize_with(at + 1, || Slot::Cold { calls: 0, loops: 0 });
        }
        if let Slot::Cold { calls, loops } | Slot::Warm { calls, loops, .. } = &mut slots[at] {
            if loop_turn { *loops += 1 } else { *calls += 1 }
            let (c, l) = (*calls, *loops);
            let loop_limit = self.threshold.saturating_mul(Self::LOOP_FACTOR);
            if c <= self.threshold && l <= loop_limit {
                if matches!(slots[at], Slot::Cold { .. }) && (c.saturating_mul(Self::WARM_DIV) > self.threshold || l.saturating_mul(Self::WARM_DIV) > loop_limit) {
                    slots[at] = Slot::Warm { calls: c, loops: l, func: func.clone() };
                }
                return Heat::Cold;
            }
            self.compile_batch(pool, slots, at, func);
        }
        let slot = &mut slots[at];
        match slot {
            Slot::Ready(r) => Heat::Ready(r),
            Slot::Cold { .. } | Slot::Warm { .. } => Heat::Cold,
            Slot::Failed => Heat::Never,
        }
    }
}

impl Tier for Jit {
    fn enter(&self, pool: &dyn Pool, id: FuncId, func: &Function, regs: &mut [Value<'_>], acc: &mut Value<'_>) -> Tiered {
        let mut slots = self.slots.borrow_mut();
        match self.heat(pool, &mut slots, id, func, false) {
            Heat::Ready(r) if r.pure => Tiered::Ran(r.code.run(regs, acc, 0)),
            // Compiled but cannot run as a whole (only switched into at loop headers), or cannot be compiled: no need
            // to ask again on frame entry
            Heat::Ready(_) | Heat::Never => Tiered::Never,
            Heat::Cold => Tiered::NotYet,
        }
    }

    fn backedge(&self, pool: &dyn Pool, id: FuncId, func: &Function, header: u32, regs: &mut [Value<'_>], acc: &mut Value<'_>) -> Tiered {
        let mut slots = self.slots.borrow_mut();
        match self.heat(pool, &mut slots, id, func, true) {
            Heat::Ready(r) if r.entries.contains(&header) => Tiered::Ran(r.code.run(regs, acc, u64::from(header) + 1)),
            Heat::Ready(r) if r.entries.is_empty() => Tiered::Never,
            Heat::Ready(_) | Heat::Cold => Tiered::NotYet,
            Heat::Never => Tiered::Never,
        }
    }
}
