//! r8executor：r8virtasm 字节码执行器（`no_std`）。
//!
//! 显式帧栈（不靠宿主递归），所有帧共用一个寄存器文件；被捕获的绑定在堆上的 [`Env`] 链里。
//! 异常：每帧一个动态处理器栈（`PushHandler` / `PopHandler`），抛出时弹本帧处理器，没有就展开到调用者。
//! 一个 `Executor` 只配一个 isolate：它装着已加载的函数，堆里的闭包按 [`FuncId`] 指回来。
#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::rc::Rc;
use core::cell::{Cell, RefCell};

use r8virtasm::{Constant, Function, FunctionKind, Insn, Mode, Module, PropertyKind, Reg};
use r8gc::{Collect, Gc};
mod ic;
mod module;

use r8framework::{Agent, Closure, Coroutine, CoroutineState, Env, Error, ErrorKind, FuncId, Invoke, IterRecord, JsEnv, JsObject, ObjectData, ObjectKind, Property, Resume, Slot, Step, Suspended, Value};

/// 调用深度上限（含宿主函数里重入的 JS），过了抛 RangeError
const STACK_LIMIT: usize = 10_000;

/// 一片最多走这么多步（进一个帧、往回跳一次各算一步：别的指令只往前走，步数之间的指令有限），片与片之间回收
#[cfg(feature = "compile")]
const SLICE: usize = 10_000;

/// 一个已加载的函数：本体加它所在模块的第一个函数编号（`CreateClosure` 的下标相对它）。
struct Loaded {
    base: u32,
    func: Function,
    /// 按常量下标：全局名上次在哪找到（读写全局名的内联缓存）
    globals: Box<[Cell<GlobalSlot>]>,
    /// JIT 层说过进帧时不用再问它
    no_tier_enter: Cell<bool>,
    /// JIT 层说过循环往回跳时不用再问它
    no_tier_loops: Cell<bool>,
    /// 按指令下标：属性读写的内联缓存（只有 `get.named` / `set.named` / `define.named` 用）
    ics: Box<[ic::Ic]>,
}

impl From<(u32, Function)> for Loaded {
    fn from((base, func): (u32, Function)) -> Self {
        let globals = func.consts.iter().map(|_| Cell::new(GlobalSlot::Unknown)).collect();
        let ics = func.insns.iter().map(|_| ic::Ic::default()).collect();
        Self { base, func, globals, no_tier_enter: Cell::new(false), no_tier_loops: Cell::new(false), ics }
    }
}

/// 全局名的位置缓存。只存属性表下标、不存 GC 指针：用之前核对那一项的键，对不上就走慢路重查。
#[derive(Clone, Copy, Debug)]
enum GlobalSlot {
    Unknown,
    /// 脚本作用域（顶层 `let` / `const` / `class`）属性表的第几项
    Scope(u32),
    /// 全局对象属性表的第几项；当时脚本作用域有几项（多了就可能被新的顶层 `let` 遮住，要重查）
    Global { at: u32, scope_len: u32 },
}

/// 一个异常处理器：落点（指令下标）与装的时候的环境。
#[derive(Collect)]
#[collect(no_drop)]
struct Handler<'gc> {
    target: u32,
    env: Option<JsEnv<'gc>>,
}

/// 一个调用帧。
#[derive(Collect)]
#[collect(no_drop)]
struct Frame<'gc> {
    func: FuncId,
    pc: usize,
    /// 寄存器文件里本帧的起点
    base: usize,
    env: Option<JsEnv<'gc>>,
    handlers: Vec<Handler<'gc>>,
    /// `new.target`（普通调用是 `undefined`）
    new_target: Value<'gc>,
    /// 构造调用的 `this`：基类构造时一进帧就有，派生类等 `super(...)` 返回
    new_this: Option<Value<'gc>>,
    /// 全部实参（只有 `keeps_args` 的函数留）
    args: Vec<Value<'gc>>,
}

/// 一次执行的全部状态（片与片之间挂在根上）。
#[derive(Collect)]
#[collect(no_drop)]
struct State<'gc> {
    regs: Vec<Value<'gc>>,
    frames: Vec<Frame<'gc>>,
    f: Frame<'gc>,
    acc: Value<'gc>,
}

impl<'gc> State<'gc> {
    /// 找处理器接住异常 `v`：本帧没有就弹帧往外找；全弹完了返回 `false`。
    fn unwind(&mut self, v: Value<'gc>) -> bool {
        loop {
            if let Some(h) = self.f.handlers.pop() {
                self.f.pc = h.target as usize;
                self.f.env = h.env;
                self.acc = v;
                return true;
            }
            match self.frames.pop() {
                Some(caller) => {
                    self.regs.truncate(self.f.base);
                    self.f = caller;
                }
                None => return false,
            }
        }
    }
}

/// 一次执行停下来的方式：返回，或协程在 `yield` / `await` 处挂起（带着存下的帧）。
enum Outcome<'gc> {
    Return(Value<'gc>),
    Suspend { value: Value<'gc>, awaiting: bool, saved: Suspended<'gc> },
    /// 指令配额用完：状态原样交回，下一片接着跑（只有分片执行的 `Engine::eval` 会用）
    #[cfg_attr(not(feature = "compile"), expect(dead_code, reason = "只有 compile feature 的 Engine::eval 分片执行"))]
    Paused(alloc::boxed::Box<State<'gc>>),
}

impl<'gc> Outcome<'gc> {
    /// 普通函数只会返回（`yield` / `await` 只在协程自己的那一帧里出现）。
    fn returned(self) -> Value<'gc> {
        match self {
            Outcome::Return(v) => v,
            Outcome::Suspend { .. } | Outcome::Paused(_) => unreachable!("only coroutines suspend, only the outermost execution pauses"),
        }
    }
}

/// 调一个对象时要做的事（先从对象里取出来，放掉借用再做）。
enum CallStep<'gc> {
    Closure(Closure<'gc>),
    Resolver,
    Proxy,
    NativeClosure(r8framework::ClosureFn),
    Native(r8framework::Native),
    /// `call` / `apply`：目标可调就展开成对它的调用
    Forward(r8framework::Native),
    Bound(Value<'gc>, Value<'gc>, Vec<Value<'gc>>),
}

/// 调用解析的结果：要进一帧的闭包，或宿主函数已经算完的值。
enum Callee<'gc> {
    Closure { closure: Closure<'gc>, callee: Value<'gc>, this: Value<'gc>, args: Vec<Value<'gc>> },
    Done(Value<'gc>),
}

/// 构造调用的准备结果：要进一帧，或宿主构造器已经给出对象。
enum Prepared<'gc> {
    Frame(Frame<'gc>),
    Done(Value<'gc>),
}

/// 比解释更快的一层（JIT）：执行器进一个普通 JS 函数（不是构造器 / 生成器 / async）的帧之前先问它。
pub trait Tier {
    /// 函数 `id`（字节码 `func`）要进帧了，本帧的寄存器是 `regs`（实参已放好）、累加器是 `acc`：
    /// 有机器码就在它们上面跑（[`Tiered::Ran`]）；还不热给 [`Tiered::NotYet`]；编不了或不能整体跑给 [`Tiered::Never`]。
    fn enter(&self, id: FuncId, func: &Function, regs: &mut [Value<'_>], acc: &mut Value<'_>) -> Tiered;

    /// 本帧往回跳到第 `header` 条指令（循环又转了一圈），寄存器与累加器是此刻的：循环热了、有机器码就从循环头
    /// 接着在机器码里跑（OSR），给出它停在哪（总是 [`Exit::Resume`]：遇到 `return` 也停下交给解释器收尾）；
    /// 不切进去给 [`Tiered::NotYet`]（以后都不会切给 [`Tiered::Never`]）。
    fn backedge(&self, id: FuncId, func: &Function, header: u32, regs: &mut [Value<'_>], acc: &mut Value<'_>) -> Tiered;
}

/// JIT 层的回答。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tiered {
    /// 在机器码里跑了，这样出来的
    Ran(Exit),
    /// 这次不跑（还不热等），以后还问
    NotYet,
    /// 这个函数（进帧 / 循环）以后都不用问了：执行器记下，不再调
    Never,
}

/// 机器码怎么出来的。机器码的返回值：0 是 [`Return`](Exit::Return)，`pc + 1` 是 [`Resume(pc)`](Exit::Resume)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// 函数返回了，结果在累加器里
    Return,
    /// 停在第 `pc` 条指令之前（还没执行它），解释器从这里接着跑
    Resume(u32),
}

impl From<u64> for Exit {
    fn from(code: u64) -> Self {
        match code {
            0 => Exit::Return,
            n => Exit::Resume(u32::try_from(n - 1).unwrap_or(u32::MAX)),
        }
    }
}

/// 字节码执行器。
#[derive(Default)]
pub struct Executor {
    /// 比解释更快的一层（没有就只解释）
    tier: Option<Rc<dyn Tier>>,
    /// 已加载的函数：只追加（`eval` / `Function` 构造器在执行中途也会加），帧里持有 `Rc`
    code: RefCell<Vec<Rc<Loaded>>>,
    /// 当前调用深度（各层重入的 `execute` 加起来）
    depth: Cell<usize>,
    /// 模块的代码：函数编号起点 ⇒ 模块编号（导入绑定、import.meta 用）
    module_of: RefCell<alloc::collections::BTreeMap<u32, u32>>,
}

impl Executor {
    /// 带上一层 JIT：普通 JS 函数进帧前先问它。
    #[must_use]
    pub fn with_tier(mut self, tier: Rc<dyn Tier>) -> Self {
        self.tier = Some(tier);
        self
    }

    /// 进帧前先问 JIT 层。函数已经在机器码里跑完（结果在 `acc`，帧的寄存器已退掉）给 `true`；
    /// 机器码中途退回就把帧的 `pc` 改到它停下的地方，照常解释。
    #[inline(never)]
    fn tiered<'gc>(&self, loaded: &Loaded, regs: &mut Vec<Value<'gc>>, f: &mut Frame<'gc>, acc: &mut Value<'gc>) -> bool {
        let Some(tier) = &self.tier else { return false };
        if loaded.no_tier_enter.get() {
            return false;
        }
        let end = f.base + usize::from(loaded.func.regs);
        match tier.enter(f.func, &loaded.func, &mut regs[f.base..end], acc) {
            Tiered::Ran(Exit::Return) => {
                regs.truncate(f.base);
                true
            }
            Tiered::Ran(Exit::Resume(pc)) => {
                f.pc = pc as usize;
                false
            }
            Tiered::NotYet => false,
            Tiered::Never => {
                loaded.no_tier_enter.set(true);
                false
            }
        }
    }

    /// 往回跳到 `header` 时问 JIT 层要不要切进机器码（OSR）；切进去了就把帧的 `pc` 改到机器码停下的地方。
    #[inline(never)]
    fn osr<'gc>(&self, loaded: &Loaded, regs: &mut [Value<'gc>], f: &mut Frame<'gc>, acc: &mut Value<'gc>, header: usize) {
        let Some(tier) = &self.tier else { return };
        if loaded.no_tier_loops.get() {
            return;
        }
        let end = f.base + usize::from(loaded.func.regs);
        let Ok(header) = u32::try_from(header) else { return };
        match tier.backedge(f.func, &loaded.func, header, &mut regs[f.base..end], acc) {
            Tiered::Ran(Exit::Resume(pc)) => f.pc = pc as usize,
            Tiered::Never => loaded.no_tier_loops.set(true),
            Tiered::Ran(Exit::Return) | Tiered::NotYet => {}
        }
    }

    /// 装进一个模块，返回脚本本身的函数编号。
    pub fn load(&self, module: Module) -> FuncId {
        let mut code = self.code.borrow_mut();
        let base = u32::try_from(code.len()).expect("fewer than 2^32 functions");
        code.extend(module.functions.into_iter().map(|func| Rc::new(Loaded::from((base, func)))));
        FuncId(base + Module::MAIN)
    }

    fn function(&self, f: FuncId) -> Rc<Loaded> {
        Rc::clone(&self.code.borrow()[f.0 as usize])
    }

    /// 跑脚本 `main`（`this` 是全局对象）。
    pub fn run_script<'gc>(&self, a: &mut Agent<'gc, '_>, main: FuncId) -> Result<Value<'gc>, Error> {
        let this = Value::Object(a.realm.global);
        let mut regs = Vec::new();
        let f = self.enter(a, &mut regs, Closure::new(main, None), Value::Undefined, this, &[]);
        self.execute(a, State { regs, frames: Vec::new(), f, acc: Value::Undefined }, None).map(Outcome::returned)
    }

    /// 直接 eval：按调用处的作用域表编译，跑在调用处的环境链上。
    #[cfg_attr(not(feature = "compile"), expect(unused_variables, reason = "没有编译器就只能报错"))]
    fn direct_eval<'gc>(&self, a: &mut Agent<'gc, '_>, src: &str, table: &r8virtasm::EvalScope, env: Option<r8framework::JsEnv<'gc>>, strict: bool, level: usize) -> Result<Value<'gc>, Error> {
        #[cfg(feature = "compile")]
        {
            let module = match Module::compile_direct(&r8framework::Source { text: src, file: "[eval]" }, strict, table) {
                Ok(m) => m,
                Err(Error::Syntax(m)) => return Err(a.throw(ErrorKind::Syntax, &m)),
                Err(e) => return Err(e),
            };
            let main = self.load(module);
            if level >= STACK_LIMIT {
                return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
            }
            let saved = self.depth.replace(level);
            let mut regs = Vec::new();
            let f = self.enter(a, &mut regs, Closure::new(main, env), Value::Undefined, Value::Undefined, &[]);
            let r = self.execute(a, State { regs, frames: Vec::new(), f, acc: Value::Undefined }, None).map(Outcome::returned);
            self.depth.set(saved);
            r
        }
        #[cfg(not(feature = "compile"))]
        Err(a.throw(ErrorKind::Syntax, "dynamic code needs the compile feature"))
    }

    /// 脚本 `main` 的初始执行状态（`this` 是全局对象）。
    #[cfg(feature = "compile")]
    fn script_state<'gc>(&self, a: &mut Agent<'gc, '_>, main: FuncId) -> State<'gc> {
        let this = Value::Object(a.realm.global);
        let mut regs = Vec::new();
        let f = self.enter(a, &mut regs, Closure::new(main, None), Value::Undefined, this, &[]);
        State { regs, frames: Vec::new(), f, acc: Value::Undefined }
    }

    /// 协程函数被调：生成器给出还没跑的生成器对象；async 函数跑到第一个 `await`，给出它的 Promise。
    fn start<'gc>(&self, a: &mut Agent<'gc, '_>, closure: Closure<'gc>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>], depth: usize) -> Result<Value<'gc>, Error> {
        let mut regs = Vec::new();
        let f = self.enter(a, &mut regs, closure, callee, this, args);
        let saved = Suspended { func: f.func, pc: 0, regs, env: f.env, handlers: Vec::new(), args: f.args, mode: None };
        let func_loaded = self.function(closure.func);
        let func = &func_loaded.func;
        let (generator, asynchronous) = (func.generator, func.asynchronous);
        let proto = if generator {
            match a.get_named(callee, "prototype")? {
                Value::Object(p) => p,
                _ if asynchronous => a.realm.async_generator_proto,
                _ => a.realm.generator_proto,
            }
        } else {
            a.realm.object_proto
        };
        let promise = (!generator).then(|| a.new_promise());
        let co = ObjectData { kind: ObjectKind::Coroutine(Box::new(Coroutine::new(saved, promise, generator && asynchronous))), proto: Some(proto), ..ObjectData::default() }.alloc(a.mc);
        let Some(p) = promise else {
            // 先跑形参初始化，停在编译器放的初始挂起点
            let outer = self.depth.replace(depth);
            let r = a.resume(co, Resume::Next, Value::Undefined);
            self.depth.set(outer);
            r?;
            return Ok(co.into());
        };
        let outer = self.depth.replace(depth);
        let r = a.drive(co, Resume::Next, Value::Undefined);
        self.depth.set(outer);
        r?;
        Ok(p.into())
    }

    /// 解开绑定函数、直接调宿主函数；剩下 JS 闭包交给调用者进帧。
    fn resolve<'gc>(&self, a: &mut Agent<'gc, '_>, mut callee: Value<'gc>, mut this: Value<'gc>, mut args: Vec<Value<'gc>>, depth: usize) -> Result<Callee<'gc>, Error> {
        while let Value::Object(o) = callee {
            // 先把要做的事取出来，借用放掉之后再调（被调的代码可能改这个函数对象）
            let step = match &o.borrow().kind {
                ObjectKind::Function(c) => CallStep::Closure(*c),
                ObjectKind::Resolver { .. } => CallStep::Resolver,
                ObjectKind::Proxy(p) if p.callable => CallStep::Proxy,
                ObjectKind::NativeClosure(c) => CallStep::NativeClosure(c.f),
                ObjectKind::Native(n) if n.forward != r8framework::Forward::No => CallStep::Forward(*n),
                ObjectKind::Native(n) => CallStep::Native(*n),
                ObjectKind::Bound(b) => CallStep::Bound(b.target, b.this, b.args.clone()),
                _ => break,
            };
            if let CallStep::Forward(n) = step
                && a.is_callable(this)
            {
                let target = this;
                let mut rest = args.into_iter();
                this = rest.next().unwrap_or_default();
                args = match n.forward {
                    r8framework::Forward::Apply => match rest.next().unwrap_or_default() {
                        Value::Undefined | Value::Null => Vec::new(),
                        list => a.list_from_array_like(list)?,
                    },
                    _ => rest.collect(),
                };
                callee = target;
                continue;
            }
            let saved = self.depth.replace(depth);
            let r = match step {
                CallStep::Closure(c) => {
                    self.depth.set(saved);
                    let loaded = self.function(c.func);
                    let f = &loaded.func;
                    if matches!(f.kind, FunctionKind::Constructor { .. }) {
                        return Err(a.throw(ErrorKind::Type, &alloc::format!("Class constructor {} cannot be invoked without 'new'", f.name)));
                    }
                    if f.generator || f.asynchronous {
                        return self.start(a, c, callee, this, &args, depth).map(Callee::Done);
                    }
                    return Ok(Callee::Closure { closure: c, callee, this, args });
                }
                CallStep::Resolver => a.run_resolver(o, args.first().copied().unwrap_or_default()),
                CallStep::Proxy => a.proxy_call(o, this, &args),
                CallStep::NativeClosure(f) => {
                    let outer = core::mem::replace(&mut a.new_target, Value::Undefined);
                    let r = f(a, o, this, &args);
                    a.new_target = outer;
                    r
                }
                CallStep::Native(n) | CallStep::Forward(n) => {
                    let outer = core::mem::replace(&mut a.new_target, Value::Undefined);
                    let r = (n.f)(a, this, &args);
                    a.new_target = outer;
                    r
                }
                CallStep::Bound(target, bound_this, mut bound_args) => {
                    self.depth.set(saved);
                    bound_args.extend(args);
                    (callee, this, args) = (target, bound_this, bound_args);
                    continue;
                }
            };
            self.depth.set(saved);
            return r.map(Callee::Done);
        }
        let what = a.describe(callee);
        Err(a.throw(ErrorKind::Type, &alloc::format!("{what} is not a function")))
    }

    /// 被调的是普通 JS 函数（不是类构造器 / 生成器 / async）就给出它的闭包：可以直接开帧。
    fn plain_closure<'gc>(&self, callee: Value<'gc>) -> Option<(Closure<'gc>, Rc<Loaded>)> {
        let Value::Object(o) = callee else { return None };
        let ObjectKind::Function(c) = o.borrow().kind else { return None };
        let loaded = self.function(c.func);
        let g = &loaded.func;
        (!g.generator && !g.asynchronous && !matches!(g.kind, FunctionKind::Constructor { .. })).then_some((c, loaded))
    }

    /// 建一帧：在寄存器文件末尾开出本函数的寄存器，按调用约定放 `this`、自身、实参。
    fn enter<'gc>(&self, a: &mut Agent<'gc, '_>, regs: &mut Vec<Value<'gc>>, closure: Closure<'gc>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Frame<'gc> {
        let f_loaded = self.function(closure.func);
        let (base, first) = Self::open_frame(a, regs, &f_loaded.func, closure, callee, this);
        let f = &f_loaded.func;
        let params = usize::from(f.params).min(args.len());
        regs[first..first + params].copy_from_slice(&args[..params]);
        let args = if f.keeps_args { args.to_vec() } else { Vec::new() };
        Frame { func: closure.func, pc: 0, base, env: closure.env, handlers: Vec::new(), new_target: Value::Undefined, new_this: None, args }
    }

    /// 同 [`enter`](Self::enter)，实参就在寄存器文件里（`Call` 的快路：不先拷成 `Vec`）。
    #[expect(clippy::too_many_arguments, reason = "调用约定的各部分")]
    fn enter_regs<'gc>(a: &mut Agent<'gc, '_>, regs: &mut Vec<Value<'gc>>, f: &Function, closure: Closure<'gc>, callee: Value<'gc>, this: Value<'gc>, args: usize, argc: usize) -> Frame<'gc> {
        let (base, first) = Self::open_frame(a, regs, f, closure, callee, this);
        let params = usize::from(f.params).min(argc);
        regs.copy_within(args..args + params, first);
        let args = if f.keeps_args { regs[args..args + argc].to_vec() } else { Vec::new() };
        Frame { func: closure.func, pc: 0, base, env: closure.env, handlers: Vec::new(), new_target: Value::Undefined, new_this: None, args }
    }

    /// 在寄存器文件末尾开出 `f` 的寄存器，放好 `this` 与自身；给出帧基址与第一个形参的位置。
    fn open_frame<'gc>(a: &mut Agent<'gc, '_>, regs: &mut Vec<Value<'gc>>, f: &Function, closure: Closure<'gc>, callee: Value<'gc>, this: Value<'gc>) -> (usize, usize) {
        a.profiler.enter(closure.func);
        let base = regs.len();
        regs.resize(base + usize::from(f.regs), Value::Undefined);
        // 非严格函数：`this` 是 null / undefined 换成全局对象，原始值包成对象（箭头函数不用自己的 `this`）
        let this = match this {
            _ if f.mode == Mode::Strict || f.kind == FunctionKind::Arrow => this,
            Value::Undefined | Value::Null => Value::Object(a.realm.global),
            Value::Object(_) | Value::Uninit => this,
            prim => a.to_object(prim).map_or(prim, Value::Object),
        };
        regs[base + usize::from(Reg::THIS.0)] = this;
        regs[base + usize::from(Reg::CALLEE.0)] = callee;
        (base, base + usize::from(Reg::PARAMS))
    }

    /// 本帧正在跑的闭包（`r1` 里的被调函数）。
    fn current<'gc>(regs: &[Value<'gc>], f: &Frame<'gc>) -> Option<Closure<'gc>> {
        match regs[f.base + usize::from(Reg::CALLEE.0)] {
            Value::Object(o) => match o.borrow().kind {
                ObjectKind::Function(c) => Some(c),
                _ => None,
            },
            _ => None,
        }
    }

    /// 准备构造调用：解开绑定函数；宿主构造器直接算；JS 构造器建好 `this`（派生类不建）并跑实例字段。
    fn prepare<'gc>(&self, a: &mut Agent<'gc, '_>, regs: &mut Vec<Value<'gc>>, mut callee: Value<'gc>, mut args: Vec<Value<'gc>>, mut new_target: Value<'gc>, depth: usize) -> Result<Prepared<'gc>, Error> {
        while let Value::Object(o) = callee {
            let kind = match &o.borrow().kind {
                ObjectKind::Function(c) => Ok(Ok(*c)),
                ObjectKind::Native(n) => Ok(Err(*n)),
                ObjectKind::Bound(b) => Err(Some((b.target, b.args.clone()))),
                ObjectKind::Proxy(p) if p.constructor => Err(None),
                _ => break,
            };
            match kind {
                Ok(Ok(closure)) => {
                    let func_loaded = self.function(closure.func);
                    let func = &func_loaded.func;
                    if func.generator || func.asynchronous {
                        break;
                    }
                    let this = match func.kind {
                        FunctionKind::Arrow | FunctionKind::Method => break,
                        FunctionKind::Constructor { derived: true } => None,
                        FunctionKind::Normal | FunctionKind::Constructor { derived: false } => {
                            let this = a.create_this(new_target)?;
                            if let Some(init) = closure.fields {
                                a.call(init.into(), this.into(), &[])?;
                            }
                            Some(Value::Object(this))
                        }
                    };
                    // 派生类构造器的 `this` 在 `super()` 之前是未初始化标记
                    let mut f = self.enter(a, regs, closure, callee, this.unwrap_or(Value::Uninit), &args);
                    f.new_target = new_target;
                    f.new_this = this;
                    return Ok(Prepared::Frame(f));
                }
                Ok(Err(native)) => {
                    if !native.constructor {
                        break;
                    }
                    // 宿主构造器自己按 `new.target` 定原型（`class X extends Map` 等）
                    let saved = self.depth.replace(depth);
                    let outer = core::mem::replace(&mut a.new_target, new_target);
                    let r = (native.f)(a, Value::Undefined, &args);
                    a.new_target = outer;
                    self.depth.set(saved);
                    let Value::Object(obj) = r? else { unreachable!("built-in constructors return objects") };
                    return Ok(Prepared::Done(obj.into()));
                }
                Err(None) => {
                    let saved = self.depth.replace(depth);
                    let r = a.proxy_construct(o, &args, new_target);
                    self.depth.set(saved);
                    return r.map(Prepared::Done);
                }
                Err(Some((target, mut bound_args))) => {
                    if matches!(new_target, Value::Object(t) if Gc::ptr_eq(t, o)) {
                        new_target = target;
                    }
                    bound_args.extend(args);
                    (callee, args) = (target, bound_args);
                }
            }
        }
        let what = a.describe(callee);
        Err(a.throw(ErrorKind::Type, &alloc::format!("{what} is not a constructor")))
    }

    /// 派生构造器里的 `super(args)`：构造父类（`new.target` 不变），初始化 `this`，跑实例字段。
    fn super_call<'gc>(a: &mut Agent<'gc, '_>, regs: &[Value<'gc>], f: &mut Frame<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        if f.new_this.is_some() {
            return Err(a.throw(ErrorKind::Reference, "Super constructor may only be called once"));
        }
        let parent = match regs[f.base + usize::from(Reg::CALLEE.0)] {
            Value::Object(o) => o.borrow().proto.map_or(Value::Null, Value::Object),
            _ => Value::Undefined,
        };
        let this = a.construct(parent, args, f.new_target)?;
        f.new_this = Some(this);
        if let Some(init) = Self::current(regs, f).and_then(|c| c.fields) {
            a.call(init.into(), this, &[])?;
        }
        Ok(this)
    }

    /// 构造调用返回时定结果：返回对象就用它，否则用 `this`（派生类没调 `super` 抛 ReferenceError）。
    fn construct_result<'gc>(&self, a: &Agent<'gc, '_>, f: &Frame<'gc>, v: Value<'gc>) -> Result<Value<'gc>, Error> {
        if matches!(v, Value::Object(_)) {
            return Ok(v);
        }
        match (self.function(f.func).func.kind, f.new_this) {
            (FunctionKind::Constructor { derived: true }, _) if v != Value::Undefined => Err(a.throw(ErrorKind::Type, "Derived constructors may only return object or undefined")),
            (FunctionKind::Constructor { derived: true }, None) => Err(a.throw(ErrorKind::Reference, "Must call super constructor in derived class before accessing 'this' or returning from derived constructor")),
            (_, Some(t)) => Ok(t),
            (_, None) => Ok(v),
        }
    }

    /// `super` 的查找起点：当前方法主对象的原型。
    fn super_base<'gc>(a: &Agent<'gc, '_>, regs: &[Value<'gc>], f: &Frame<'gc>) -> Result<Value<'gc>, Error> {
        let home = Self::current(regs, f).and_then(|c| c.home).ok_or_else(|| a.throw(ErrorKind::Syntax, "'super' keyword unexpected here"))?;
        Ok(home.borrow().proto.map_or(Value::Null, Value::Object))
    }

    /// 建一个函数对象；普通函数带 `prototype`（`constructor` 指回来）。
    fn function_object<'gc>(&self, a: &Agent<'gc, '_>, closure: Closure<'gc>, proto: JsObject<'gc>) -> JsObject<'gc> {
        let func_loaded = self.function(closure.func);
        let func = &func_loaded.func;
        // 缺省原型按函数种类：生成器 / async / async 生成器各有自己的 %XFunction.prototype%
        let proto = match (Gc::ptr_eq(proto, a.realm.function_proto), func.generator, func.asynchronous) {
            (true, true, true) => a.realm.async_generator_function_proto,
            (true, true, false) => a.realm.generator_function_proto,
            (true, false, true) => a.realm.async_function_proto,
            _ => proto,
        };
        let fo = ObjectData { kind: ObjectKind::Function(closure), proto: Some(proto), ..ObjectData::default() }.alloc(a.mc);
        a.realm.name_function(a.mc, fo, &func.name, Some(u32::from(func.length)));
        if func.generator {
            // 生成器函数的 `prototype` 是它产出的生成器的原型（没有 `constructor`）
            let base = if func.asynchronous { a.realm.async_generator_proto } else { a.realm.generator_proto };
            let p = ObjectData { proto: Some(base), ..ObjectData::default() }.alloc(a.mc);
            fo.borrow_mut(a.mc).define("prototype", Property { slot: Slot::Data(p.into()), enumerable: false, writable: true, configurable: false });
        } else if func.kind == FunctionKind::Normal && !func.asynchronous {
            let p = a.object();
            p.borrow_mut(a.mc).define("constructor", Property::hidden(fo.into()));
            fo.borrow_mut(a.mc).define("prototype", Property { slot: Slot::Data(p.into()), enumerable: false, writable: true, configurable: false });
        }
        fo
    }

    /// `class … extends parent`：原型对象的原型与构造器的原型。
    fn class_parents<'gc>(a: &mut Agent<'gc, '_>, parent: Option<Value<'gc>>) -> Result<(Option<JsObject<'gc>>, JsObject<'gc>), Error> {
        match parent {
            None => Ok((Some(a.realm.object_proto), a.realm.function_proto)),
            Some(Value::Null) => Ok((None, a.realm.function_proto)),
            Some(p @ Value::Object(o)) if o.borrow().kind.is_callable() => match a.get_named(p, "prototype")? {
                Value::Object(pp) => Ok((Some(pp), o)),
                Value::Null => Ok((None, o)),
                _ => Err(a.throw(ErrorKind::Type, "Class extends value does not have valid prototype property")),
            },
            Some(p) => {
                let what = a.to_string(p)?;
                Err(a.throw(ErrorKind::Type, &alloc::format!("Class extends value {} is not a constructor or null", &**what)))
            }
        }
    }

    /// 往外第 `depth` 层环境。
    fn env_at(env: Option<JsEnv<'_>>, depth: u16) -> JsEnv<'_> {
        let mut e = env.expect("a scope with an environment");
        for _ in 0..depth {
            e = e.borrow().parent.expect("an enclosing environment");
        }
        e
    }

    /// %ThrowTypeError%：严格模式 `arguments.callee` 的访问器。
    fn throw_type_error<'gc>(a: &mut Agent<'gc, '_>, _: Value<'gc>, _: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        Err(a.throw(ErrorKind::Type, "'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions or the arguments objects for calls to them"))
    }

    /// 严格模式下删不掉（不可配置）抛 TypeError。
    fn check_delete(a: &Agent<'_, '_>, ok: bool, strict: bool) -> Result<(), Error> {
        if strict && !ok {
            return Err(a.throw(ErrorKind::Type, "Cannot delete a non-configurable property"));
        }
        Ok(())
    }

    /// 未初始化就抛 ReferenceError（暂时性死区）。
    #[inline]
    fn check<'gc>(a: &Agent<'gc, '_>, v: Value<'gc>, name: &str) -> Result<(), Error> {
        if matches!(v, Value::Uninit) {
            return Err(Self::uninitialized(a, name));
        }
        Ok(())
    }

    /// 死区指令的检查：名字只在真要抛错时才从常量池取。
    #[inline]
    fn check_const<'gc>(a: &Agent<'gc, '_>, v: Value<'gc>, func: &Function, name: r8virtasm::Const) -> Result<(), Error> {
        if matches!(v, Value::Uninit) {
            return Err(Self::uninitialized(a, func.name_at(name)));
        }
        Ok(())
    }

    #[cold]
    fn uninitialized(a: &Agent<'_, '_>, name: &str) -> Error {
        a.throw(ErrorKind::Reference, &alloc::format!("Cannot access '{name}' before initialization"))
    }

    /// 按缓存读全局名：命中给出值（脚本作用域的还要查死区），没命中给 `None`。
    fn global_cached<'gc>(a: &Agent<'gc, '_>, slot: GlobalSlot, name: &str) -> Option<Value<'gc>> {
        let scope = a.realm.script_scope.borrow();
        match slot {
            GlobalSlot::Scope(at) => scope.prop_at(at as usize, name).and_then(|p| p.value()),
            GlobalSlot::Global { at, scope_len } if scope.props.len() == scope_len as usize => a.realm.global.borrow().prop_at(at as usize, name).and_then(|p| p.value()),
            _ => None,
        }
    }

    /// 全局名现在在哪（自有数据属性才缓存）。
    fn global_locate(a: &Agent<'_, '_>, name: &str) -> GlobalSlot {
        let scope = a.realm.script_scope.borrow();
        let own = |d: &r8framework::ObjectData<'_>| d.position(name).filter(|&i| matches!(d.props.slot(i), Slot::Data(_))).and_then(|i| u32::try_from(i).ok());
        if let Some(at) = own(&scope) {
            return GlobalSlot::Scope(at);
        }
        if scope.position(name).is_some() {
            return GlobalSlot::Unknown;
        }
        match (own(&a.realm.global.borrow()), u32::try_from(scope.props.len())) {
            (Some(at), Ok(scope_len)) => GlobalSlot::Global { at, scope_len },
            _ => GlobalSlot::Unknown,
        }
    }

    /// `lda.global`：先按缓存，没命中走慢路再记下位置。
    fn global_load<'gc>(a: &mut Agent<'gc, '_>, cache: &Cell<GlobalSlot>, name: &str) -> Result<Value<'gc>, Error> {
        if let Some(v) = Self::global_cached(a, cache.get(), name) {
            Self::check(a, v, name)?;
            return Ok(v);
        }
        let v = Self::global_get(a, name)?.ok_or_else(|| a.throw(ErrorKind::Reference, &alloc::format!("{name} is not defined")))?;
        cache.set(Self::global_locate(a, name));
        Ok(v)
    }

    /// `sta.global`：缓存命中且是可写数据属性就原地改（脚本作用域的还要查死区），否则走慢路再记下位置。
    fn global_store<'gc>(a: &mut Agent<'gc, '_>, cache: &Cell<GlobalSlot>, name: &str, v: Value<'gc>, strict: bool) -> Result<(), Error> {
        let (target, at) = match cache.get() {
            GlobalSlot::Scope(at) => (a.realm.script_scope, at),
            GlobalSlot::Global { at, scope_len } if a.realm.script_scope.borrow().props.len() == scope_len as usize => (a.realm.global, at),
            _ => (a.realm.global, u32::MAX),
        };
        let hit = target.borrow().prop_at(at as usize, name);
        if let Some(Property { slot: Slot::Data(old), writable: true, .. }) = hit {
            Self::check(a, old, name)?;
            target.borrow_mut(a.mc).set_at(at as usize, v);
            return Ok(());
        }
        Self::global_set(a, name, v, strict)?;
        cache.set(Self::global_locate(a, name));
        Ok(())
    }

    /// 读全局名字：先脚本作用域（死区里抛错），再全局对象（含原型链）；都没有给 `None`。
    fn global_get<'gc>(a: &mut Agent<'gc, '_>, name: &str) -> Result<Option<Value<'gc>>, Error> {
        if let Some(v) = a.realm.script_scope.borrow().own(name) {
            Self::check(a, v, name)?;
            return Ok(Some(v));
        }
        let g = Value::Object(a.realm.global);
        if a.has_property(g, name)? { Ok(Some(a.get_named(g, name)?)) } else { Ok(None) }
    }

    /// 顶层 `var` / 函数声明之前查能不能声明（CanDeclareGlobalVar / CanDeclareGlobalFunction）。
    fn check_declare(a: &mut Agent<'_, '_>, name: &str, function: bool) -> Result<(), Error> {
        if a.realm.script_scope.borrow().get_own(name).is_some() {
            return Err(a.throw(ErrorKind::Syntax, &alloc::format!("Identifier '{name}' has already been declared")));
        }
        let (existing, extensible) = {
            let g = a.realm.global.borrow();
            (g.get_own(name), !g.non_extensible)
        };
        match existing {
            Some(p) if function && !p.configurable && !(p.writable && p.enumerable && matches!(p.slot, Slot::Data(_))) => Err(a.throw(ErrorKind::Type, &alloc::format!("Cannot redefine property: {name}"))),
            None if !extensible => Err(a.throw(ErrorKind::Type, &alloc::format!("Cannot declare global {name}: the global object is not extensible"))),
            _ => Ok(()),
        }
    }

    /// 顶层 `var`（`value` 为空）/ 函数声明落到全局对象上（已经查过能声明）。
    fn declare_global<'gc>(a: &Agent<'gc, '_>, name: &str, value: Option<Value<'gc>>, deletable: bool) {
        let global = a.realm.global;
        let existing = global.borrow().get_own(name);
        let fresh = Property { slot: Slot::Data(value.unwrap_or_default()), enumerable: true, writable: true, configurable: deletable };
        let prop = match (existing, value) {
            (None, _) => fresh,
            (Some(_), None) => return,
            (Some(p), Some(_)) if p.configurable => fresh,
            (Some(p), Some(v)) => Property { slot: Slot::Data(v), ..p },
        };
        global.borrow_mut(a.mc).define(name, prop);
    }

    /// 写全局名字：脚本作用域里的查死区与 `const`，否则写全局对象。
    /// 严格模式下给没声明过的名字赋值抛 ReferenceError。
    fn global_set<'gc>(a: &mut Agent<'gc, '_>, name: &str, v: Value<'gc>, strict: bool) -> Result<(), Error> {
        let scope = a.realm.script_scope;
        let prop = scope.borrow().get_own(name);
        match prop {
            Some(p) => {
                Self::check(a, p.value().unwrap_or_default(), name)?;
                if !p.writable {
                    return Err(a.throw(ErrorKind::Type, "Assignment to constant variable."));
                }
                scope.borrow_mut(a.mc).set(name, v);
                Ok(())
            }
            None => {
                let g = Value::Object(a.realm.global);
                if strict && !a.has_property(g, name)? {
                    return Err(a.throw(ErrorKind::Reference, &alloc::format!("{name} is not defined")));
                }
                a.put_named(g, name, v, strict)
            }
        }
    }

    /// 编译器放进寄存器的私有名。
    fn private_key(v: Value<'_>) -> r8framework::JsSymbol<'_> {
        match v {
            Value::Symbol(s) => s,
            _ => unreachable!("the compiler loads a private name here"),
        }
    }

    /// 本帧的 arguments 对象：全部实参作可枚举下标属性，`length` / `callee` / `[Symbol.iterator]` 不可枚举。
    fn arguments<'gc>(a: &mut Agent<'gc, '_>, regs: &[Value<'gc>], f: &Frame<'gc>, self_strict: bool, mapped: &[Option<u16>]) -> Result<Value<'gc>, Error> {
        // 映射：只到实参个数为止
        let map = match (self_strict, f.env) {
            (false, Some(env)) if !mapped.is_empty() => Some(r8framework::ArgumentsMap { env, slots: mapped.iter().take(f.args.len()).copied().collect() }),
            _ => None,
        };
        let o = ObjectData { kind: ObjectKind::Arguments(map), proto: Some(a.realm.object_proto), ..ObjectData::default() }.alloc(a.mc);
        let hidden = |v: Value<'gc>| Property { slot: Slot::Data(v), enumerable: false, writable: true, configurable: true };
        for (i, &v) in f.args.iter().enumerate() {
            let k = alloc::string::String::from(ryu_js::Buffer::new().format(f64::from(u32::try_from(i).unwrap_or(u32::MAX))));
            o.borrow_mut(a.mc).define(&k, Property::data(v));
        }
        let len = f64::from(u32::try_from(f.args.len()).unwrap_or(u32::MAX));
        let values = a.get_symbol(Value::Object(a.realm.array_proto), a.realm.symbols.iterator, Value::Undefined)?;
        let mut d = o.borrow_mut(a.mc);
        d.define("length", hidden(len.into()));
        if self_strict {
            // 严格模式：读写 callee 都抛 TypeError
            let thrower = a.realm.named_native(a.mc, Self::throw_type_error, "", 0);
            d.define("callee", Property { slot: Slot::Accessor { get: Some(thrower), set: Some(thrower) }, enumerable: false, writable: false, configurable: false });
        } else {
            d.define("callee", hidden(regs[f.base + usize::from(Reg::CALLEE.0)]));
        }
        d.define_symbol(a.realm.symbols.iterator, hidden(values));
        drop(d);
        Ok(o.into())
    }

    /// 寄存器里的恢复方式。
    fn mode(v: Value<'_>) -> Resume {
        match v {
            Value::Number(x) => Resume::ALL.into_iter().find(|&m| f64::from(m) == x).unwrap_or(Resume::Next),
            _ => Resume::Next,
        }
    }

    /// 数组的元素（快照）；不是数组给空。
    fn items(v: Value<'_>) -> Vec<Value<'_>> {
        match v {
            Value::Object(o) => match &o.borrow().kind {
                ObjectKind::Array(items) => items.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// 往编译器建的数组末尾追加。
    fn push<'gc>(a: &Agent<'gc, '_>, arr: Value<'gc>, items: impl IntoIterator<Item = Value<'gc>>) {
        let Value::Object(o) = arr else { return };
        let mut d = o.borrow_mut(a.mc);
        for v in items {
            let len = d.array_length().unwrap_or(0);
            match &mut d.kind {
                ObjectKind::Array(dense) if dense.len() == len as usize => dense.push(v),
                _ => d.store(r8framework::Key::Str(&alloc::format!("{len}")), Property::data(v)),
            }
        }
    }

    /// 对象字面量 / 类里定义一个属性（值或访问器函数是 `v`）。
    fn define<'gc>(a: &mut Agent<'gc, '_>, obj: Value<'gc>, key: &str, kind: PropertyKind, enumerable: bool, v: Value<'gc>) -> Result<(), Error> {
        let Value::Object(o) = obj else { return Ok(()) };
        match (kind, v) {
            (PropertyKind::Data, _) => a.define(o, key, Property { slot: Slot::Data(v), enumerable, writable: true, configurable: true })?,
            (PropertyKind::Getter | PropertyKind::Setter, Value::Object(func)) => a.define_accessor(o, key, func, kind == PropertyKind::Setter, enumerable),
            _ => {}
        }
        Ok(())
    }

    /// 跑到最外层帧返回；异常先找处理器，接不住才往外抛。
    /// `budget` 只给最外层：用完就带着状态停下。
    fn execute<'gc>(&self, a: &mut Agent<'gc, '_>, mut st: State<'gc>, mut budget: Option<usize>) -> Result<Outcome<'gc>, Error> {
        let base = self.depth.get();
        let r = loop {
            match self.run(a, &mut st, base, &mut budget) {
                Ok(Some(v)) => break Ok(v),
                Ok(None) => break Ok(Outcome::Paused(alloc::boxed::Box::new(st))),
                Err(Error::Exception(g)) => {
                    let v = a.local(&g);
                    if !st.unwind(v) {
                        break Err(Error::Exception(g));
                    }
                }
                Err(e) => break Err(e),
            }
        };
        self.depth.set(base);
        r
    }

    /// 配额扣一步；用完了返回 `true`（调用方停下，状态完整）
    #[inline]
    fn spent(budget: &mut Option<usize>) -> bool {
        match budget {
            Some(0) => true,
            Some(b) => {
                *b -= 1;
                false
            }
            None => false,
        }
    }

    /// 主循环：外层每进一个帧（开始、调用、返回）取一次函数，内层逐条取指令分派；
    /// 换帧的指令 `continue 'frame`。出错直接返回，由 [`execute`](Self::execute) 展开。
    #[expect(clippy::too_many_lines, reason = "一条指令一个分支，拆开反而难读")]
    fn run<'gc>(&self, a: &mut Agent<'gc, '_>, st: &mut State<'gc>, depth: usize, budget: &mut Option<usize>) -> Result<Option<Outcome<'gc>>, Error> {
        let State { regs, frames, f, acc } = st;
        'frame: loop {
            // 配额用完：停在这一帧的下一条指令之前，状态完整
            if Self::spent(budget) {
                return Ok(None);
            }
            let cur = self.function(f.func);
            let loaded = &*cur;
            let func = &loaded.func;
            loop {
                let insn = func.insns[f.pc];
                f.pc += 1;
                let r = |r: Reg| f.base + usize::from(r.0);
                match insn {
                    Insn::LdaUndefined => *acc = Value::Undefined,
                    Insn::LdaNull => *acc = Value::Null,
                    Insn::LdaBool(b) => *acc = b.into(),
                    Insn::LdaSmi(n) => *acc = f64::from(n).into(),
                    Insn::LdaConst(k) => {
                        *acc = match &func.consts[k.0 as usize] {
                            Constant::Number(x) => (*x).into(),
                            Constant::String(s) => a.string(s).into(),
                            Constant::BigInt(digits) => a.bigint(Agent::parse_bigint(digits).unwrap_or_default()),
                        }
                    }
                    Insn::Ldar(x) => *acc = regs[r(x)],
                    Insn::Star(x) => regs[r(x)] = *acc,
                    Insn::LdaGlobal(k) => *acc = Self::global_load(a, &loaded.globals[k.0 as usize], func.name_at(k))?,
                    Insn::LdaGlobalOrUndefined(k) => *acc = Self::global_get(a, func.name_at(k))?.unwrap_or_default(),
                    Insn::StaGlobal { name, strict } => Self::global_store(a, &loaded.globals[name.0 as usize], func.name_at(name), *acc, strict)?,
                    Insn::LdaHole => *acc = Value::Uninit,
                    Insn::ThrowConstAssign => return Err(a.throw(ErrorKind::Type, "Assignment to constant variable.")),
                    Insn::CheckInit { name } => Self::check_const(a, *acc, func, name)?,
                    Insn::CheckReg { reg, name } => Self::check_const(a, regs[r(reg)], func, name)?,
                    Insn::CheckEnv { depth, slot, name } => {
                        let v = Self::env_at(f.env, depth).borrow().slots[usize::from(slot)];
                        Self::check_const(a, v, func, name)?;
                    }
                    Insn::CheckThis => {
                        if matches!(*acc, Value::Uninit) {
                            return Err(a.throw(ErrorKind::Reference, "Must call super constructor in derived class before accessing 'this' or returning from derived constructor"));
                        }
                    }
                    Insn::DeclareLexical { name, constant } => {
                        let name = func.name_at(name);
                        let scope = a.realm.script_scope;
                        // 和脚本作用域里的名字、不可配置的全局属性（以前的 `var`、`undefined` 等）都冲突
                        if scope.borrow().get_own(name).is_some() || a.realm.global.borrow().get_own(name).is_some_and(|p| !p.configurable) {
                            return Err(a.throw(ErrorKind::Syntax, &alloc::format!("Identifier '{name}' has already been declared")));
                        }
                        scope.borrow_mut(a.mc).define(name, Property { slot: Slot::Data(Value::Uninit), enumerable: false, writable: !constant, configurable: false });
                    }
                    Insn::CheckDeclare { name, function } => Self::check_declare(a, func.name_at(name), function)?,
                    Insn::DeclareVar { name, deletable } => Self::declare_global(a, func.name_at(name), None, deletable),
                    Insn::DeclareFunction { name, deletable } => Self::declare_global(a, func.name_at(name), Some(*acc), deletable),
                    Insn::SetProtoLiteral { obj } => {
                        if let Value::Object(o) = regs[r(obj)] {
                            match *acc {
                                Value::Object(p) => o.borrow_mut(a.mc).proto = Some(p),
                                Value::Null => o.borrow_mut(a.mc).proto = None,
                                _ => {}
                            }
                        }
                    }
                    Insn::ToObject => *acc = a.to_object(*acc)?.into(),
                    Insn::LdaImport { index } => *acc = self.import_value(a, loaded.base, index)?,
                    Insn::ImportMeta => *acc = self.import_meta(a, loaded.base)?,
                    Insn::DynamicImport { referrer } => {
                        let level = depth + frames.len() + 1;
                        *acc = self.dynamic_import(a, func.name_at(referrer), *acc, level)?;
                    }
                    Insn::CreateNullObject => *acc = ObjectData::default().alloc(a.mc).into(),
                    Insn::DeclareEvalVar { obj, name } => {
                        if let Value::Object(o) = regs[r(obj)] {
                            let key = r8framework::Key::Str(func.name_at(name));
                            if !a.has_own_property(o, key)? {
                                a.create_data_property(o, key, Value::Undefined)?;
                            }
                        }
                    }
                    Insn::WithHas { obj, name } => {
                        let o = regs[r(obj)];
                        let key = func.name_at(name);
                        let mut found = matches!(o, Value::Object(x) if a.has_key(x, r8framework::Key::Str(key))?);
                        if found {
                            let u = a.get_symbol(o, a.realm.symbols.unscopables, o)?;
                            if matches!(u, Value::Object(_)) {
                                let blocked = a.get_named(u, key)?;
                                found = !a.to_boolean(blocked);
                            }
                        }
                        *acc = found.into();
                    }
                    Insn::InitLexical { name } => {
                        let name = func.name_at(name);
                        let scope = a.realm.script_scope;
                        let mut d = scope.borrow_mut(a.mc);
                        let prop = d.get_own(name).map(|p| Property { slot: Slot::Data(*acc), ..p });
                        if let Some(p) = prop {
                            d.define(name, p);
                        }
                    }
                    Insn::LdaEnv { depth, slot } => *acc = Self::env_at(f.env, depth).borrow().slots[usize::from(slot)],
                    Insn::StaEnv { depth, slot } => Self::env_at(f.env, depth).borrow_mut(a.mc).slots[usize::from(slot)] = *acc,
                    Insn::PushEnv(n) => f.env = Some(Env { slots: alloc::vec![Value::Undefined; usize::from(n)], parent: f.env }.alloc(a.mc)),
                    Insn::PopEnv => f.env = Self::env_at(f.env, 0).borrow().parent,
                    Insn::CloneEnv => {
                        let e = Self::env_at(f.env, 0);
                        let copy = Env { slots: e.borrow().slots.clone(), parent: e.borrow().parent };
                        f.env = Some(copy.alloc(a.mc));
                    }
                    // 两边都是数的快路就地算，别的交给 Agent
                    Insn::Binary(op, x) => {
                        *acc = match (regs[r(x)], *acc) {
                            (Value::Number(lhs), Value::Number(rhs)) => op.numbers(lhs, rhs).into(),
                            (lhs, rhs) => a.binary(op, lhs, rhs)?,
                        }
                    }
                    Insn::Compare(op, x) => {
                        *acc = match (regs[r(x)], *acc) {
                            (Value::Number(lhs), Value::Number(rhs)) => op.numbers(lhs, rhs),
                            (lhs, rhs) => a.compare(op, lhs, rhs)?,
                        }
                        .into();
                    }
                    Insn::Unary(op) => *acc = a.unary(op, *acc)?,
                    Insn::ToNumeric => {
                        if !matches!(*acc, Value::Number(_)) {
                            *acc = match a.to_numeric(*acc)? {
                                r8framework::Numeric::Number(x) => x.into(),
                                r8framework::Numeric::BigInt(b) => Value::BigInt(b),
                            }
                        }
                    }
                    Insn::Inc => *acc = if let Value::Number(x) = *acc { (x + 1.0).into() } else { a.increment(*acc, 1)? },
                    Insn::Dec => *acc = if let Value::Number(x) = *acc { (x - 1.0).into() } else { a.increment(*acc, -1)? },
                    Insn::CreateRegExp { pattern, flags } => {
                        let (p, fl) = (a.string(func.name_at(pattern)), a.string(func.name_at(flags)));
                        let ctor = a.realm.regexp_ctor;
                        *acc = a.call(ctor.into(), Value::Undefined, &[p.into(), fl.into()])?;
                    }
                    // 往回跳（目标不在这条之后）是循环又转一圈：有 JIT 层就问它要不要从循环头切进机器码
                    Insn::Jump(l) => {
                        let back = (l.0 as usize) < f.pc;
                        f.pc = l.0 as usize;
                        if back {
                            if self.tier.is_some() {
                                self.osr(loaded, regs, f, acc, f.pc);
                            }
                            if Self::spent(budget) {
                                return Ok(None);
                            }
                        }
                    }
                    Insn::JumpIfTrue(l) => {
                        if a.to_boolean(*acc) {
                            let back = (l.0 as usize) < f.pc;
                            f.pc = l.0 as usize;
                            if back {
                                if self.tier.is_some() {
                                    self.osr(loaded, regs, f, acc, f.pc);
                                }
                                if Self::spent(budget) {
                                    return Ok(None);
                                }
                            }
                        }
                    }
                    Insn::JumpIfFalse(l) => {
                        if !a.to_boolean(*acc) {
                            let back = (l.0 as usize) < f.pc;
                            f.pc = l.0 as usize;
                            if back {
                                if self.tier.is_some() {
                                    self.osr(loaded, regs, f, acc, f.pc);
                                }
                                if Self::spent(budget) {
                                    return Ok(None);
                                }
                            }
                        }
                    }
                    Insn::JumpIfNotNullish(l) => {
                        if !acc.is_nullish() {
                            f.pc = l.0 as usize;
                        }
                    }
                    Insn::JumpIfNullish(l) => {
                        if acc.is_nullish() {
                            f.pc = l.0 as usize;
                        }
                    }
                    Insn::JumpIfUndefined(l) => {
                        if matches!(*acc, Value::Undefined) {
                            f.pc = l.0 as usize;
                        }
                    }
                    Insn::JumpIfNotUndefined(l) => {
                        if !matches!(*acc, Value::Undefined) {
                            f.pc = l.0 as usize;
                        }
                    }
                    Insn::CreateRest(from) => *acc = a.array(f.args.get(usize::from(from)..).unwrap_or(&[]).to_vec()).into(),
                    Insn::ArrayPush { arr } => Self::push(a, regs[r(arr)], [*acc]),
                    Insn::ArrayHole { arr } => {
                        if let Value::Object(o) = regs[r(arr)] {
                            let mut d = o.borrow_mut(a.mc);
                            let len = d.array_length().unwrap_or(0);
                            d.set_array_length(len + 1);
                        }
                    }
                    Insn::ArraySpread { arr } => {
                        let items = a.to_list(*acc)?;
                        Self::push(a, regs[r(arr)], items);
                    }
                    Insn::CallSpread { callee, this, args } => {
                        let level = depth + frames.len() + 1;
                        if level >= STACK_LIMIT {
                            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
                        }
                        let list = Self::items(regs[r(args)]);
                        match self.resolve(a, regs[r(callee)], regs[r(this)], list, level)? {
                            Callee::Closure { closure, callee, this, args } => {
                                let next = self.enter(a, regs, closure, callee, this, &args);
                                frames.push(core::mem::replace(f, next));
                                continue 'frame;
                            }
                            Callee::Done(v) => *acc = v,
                        }
                    }
                    Insn::ConstructSpread { callee, args } => {
                        let level = depth + frames.len() + 1;
                        if level >= STACK_LIMIT {
                            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
                        }
                        let list = Self::items(regs[r(args)]);
                        let c = regs[r(callee)];
                        match self.prepare(a, regs, c, list, c, level)? {
                            Prepared::Frame(next) => {
                                frames.push(core::mem::replace(f, next));
                                continue 'frame;
                            }
                            Prepared::Done(v) => *acc = v,
                        }
                    }
                    Insn::SuperCallSpread { args } => {
                        let list = Self::items(regs[r(args)]);
                        *acc = Self::super_call(a, regs, f, &list)?;
                    }
                    Insn::CopyData { obj } => {
                        if let Value::Object(o) = regs[r(obj)] {
                            a.copy_data(o, *acc, &[])?;
                        }
                    }
                    Insn::ObjectRest { src, keys, count } => {
                        let first = r(keys);
                        let exclude = regs[first..first + usize::from(count)].iter().filter(|k| !matches!(k, Value::Symbol(_))).map(|&k| a.property_key(k)).collect::<Result<Vec<_>, _>>()?;
                        let o = a.object();
                        a.copy_data(o, regs[r(src)], &exclude)?;
                        *acc = o.into();
                    }
                    Insn::CreateClosure(n) => {
                        let mut closure = Closure::new(FuncId(loaded.base + n), f.env);
                        if self.function(closure.func).func.kind == FunctionKind::Arrow {
                            closure.home = Self::current(regs, f).and_then(|c| c.home);
                        }
                        *acc = self.function_object(a, closure, a.realm.function_proto).into();
                    }
                    Insn::CreateMethod { func: n, home } => {
                        let mut closure = Closure::new(FuncId(loaded.base + n), f.env);
                        if let Value::Object(h) = regs[r(home)] {
                            closure.home = Some(h);
                        }
                        *acc = self.function_object(a, closure, a.realm.function_proto).into();
                    }
                    Insn::CreateClass { func: n, parent, proto } => {
                        let (proto_parent, ctor_parent) = Self::class_parents(a, parent.map(|p| regs[r(p)]))?;
                        let p = ObjectData { proto: proto_parent, ..ObjectData::default() }.alloc(a.mc);
                        let mut closure = Closure::new(FuncId(loaded.base + n), f.env);
                        closure.home = Some(p);
                        let ctor = self.function_object(a, closure, ctor_parent);
                        ctor.borrow_mut(a.mc).define("prototype", Property { slot: Slot::Data(p.into()), enumerable: false, writable: false, configurable: false });
                        p.borrow_mut(a.mc).define("constructor", Property::hidden(ctor.into()));
                        regs[r(proto)] = p.into();
                        *acc = ctor.into();
                    }
                    Insn::SetClassFields { ctor } => {
                        if let (Value::Object(c), Value::Object(init)) = (regs[r(ctor)], *acc)
                            && let ObjectKind::Function(closure) = &mut c.borrow_mut(a.mc).kind
                        {
                            closure.fields = Some(init);
                        }
                    }
                    Insn::DefineNamed { obj, name, kind, enumerable } => {
                        let ic = &loaded.ics[f.pc - 1];
                        match regs[r(obj)] {
                            Value::Object(o) if kind == PropertyKind::Data => {
                                if !ic.set(a.mc, o, *acc, false) {
                                    let before = ic::Before::from(o);
                                    Self::define(a, regs[r(obj)], func.name_at(name), kind, enumerable, *acc)?;
                                    ic.fill_set(o, func.name_at(name), before, false);
                                }
                            }
                            target => Self::define(a, target, func.name_at(name), kind, enumerable, *acc)?,
                        }
                    }
                    Insn::DefineKeyed { obj, key, kind, enumerable } => {
                        if let Value::Object(o) = regs[r(obj)] {
                            let k = regs[r(key)];
                            match (kind, *acc) {
                                (PropertyKind::Data, v) => a.define_keyed(o, k, Property { slot: Slot::Data(v), enumerable, writable: true, configurable: true })?,
                                (PropertyKind::Getter | PropertyKind::Setter, Value::Object(func)) => a.define_accessor_keyed(o, k, func, kind == PropertyKind::Setter, enumerable)?,
                                _ => {}
                            }
                        }
                    }
                    Insn::GetSuper { this, name } => {
                        let base = Self::super_base(a, regs, f)?;
                        *acc = a.get(base, func.name_at(name), regs[r(this)])?;
                    }
                    Insn::GetSuperKeyed { this } => {
                        let base = Self::super_base(a, regs, f)?;
                        let k = a.property_key(*acc)?;
                        *acc = a.get(base, &k, regs[r(this)])?;
                    }
                    Insn::SuperCall { args, argc } => {
                        let first = r(args);
                        let list = regs[first..first + usize::from(argc)].to_vec();
                        *acc = Self::super_call(a, regs, f, &list)?;
                    }
                    Insn::SuperCallForward => {
                        let list = core::mem::take(&mut f.args);
                        *acc = Self::super_call(a, regs, f, &list)?;
                    }
                    Insn::Construct { callee, args, argc } => {
                        let level = depth + frames.len() + 1;
                        if level >= STACK_LIMIT {
                            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
                        }
                        let first = r(args);
                        let list = regs[first..first + usize::from(argc)].to_vec();
                        let c = regs[r(callee)];
                        match self.prepare(a, regs, c, list, c, level)? {
                            Prepared::Frame(next) => {
                                frames.push(core::mem::replace(f, next));
                                continue 'frame;
                            }
                            Prepared::Done(v) => *acc = v,
                        }
                    }
                    Insn::InstanceOf(x) => *acc = a.instance_of(regs[r(x)], *acc)?.into(),
                    Insn::In(x) => *acc = a.has_keyed(*acc, regs[r(x)])?.into(),
                    Insn::DeleteNamed { obj, name, strict } => {
                        let ok = a.delete(regs[r(obj)], func.name_at(name))?;
                        Self::check_delete(a, ok, strict)?;
                        *acc = ok.into();
                    }
                    Insn::DeleteKeyed { obj, strict } => {
                        let ok = a.delete_keyed(regs[r(obj)], *acc)?;
                        Self::check_delete(a, ok, strict)?;
                        *acc = ok.into();
                    }
                    Insn::CreateObject => *acc = a.object().into(),
                    Insn::CreateArray { first, count } => *acc = a.array(regs[r(first)..r(first) + usize::from(count)].to_vec()).into(),
                    Insn::GetNamed { obj, name } => {
                        let ic = &loaded.ics[f.pc - 1];
                        *acc = match regs[r(obj)] {
                            Value::Object(o) => match ic.get(o) {
                                Some(v) => v,
                                None => {
                                    let v = a.get_named(Value::Object(o), func.name_at(name))?;
                                    ic.fill_get(o, func.name_at(name));
                                    v
                                }
                            },
                            target => a.get_named(target, func.name_at(name))?,
                        };
                    }
                    Insn::SetNamed { obj, name, strict } => {
                        let ic = &loaded.ics[f.pc - 1];
                        match regs[r(obj)] {
                            Value::Object(o) => {
                                if !ic.set(a.mc, o, *acc, true) {
                                    let before = ic::Before::from(o);
                                    a.put_named(Value::Object(o), func.name_at(name), *acc, strict)?;
                                    ic.fill_set(o, func.name_at(name), before, true);
                                }
                            }
                            target => a.put_named(target, func.name_at(name), *acc, strict)?,
                        }
                    }
                    Insn::GetKeyed { obj } => *acc = a.get_keyed(regs[r(obj)], *acc)?,
                    Insn::SetKeyed { obj, key, strict } => a.put_keyed(regs[r(obj)], regs[r(key)], *acc, strict)?,
                    Insn::CallEval { callee, args, argc, scope, .. } if matches!(regs[r(callee)], Value::Object(o) if Gc::ptr_eq(o, a.realm.eval)) => {
                        let first = if argc == 0 { None } else { Some(regs[r(args)]) };
                        *acc = match first {
                            None => Value::Undefined,
                            Some(Value::String(s)) => {
                                let src = alloc::string::String::from(&**s);
                                let table = &func.scopes[usize::from(scope)];
                                self.direct_eval(a, &src, table, f.env, func.mode == Mode::Strict, depth + frames.len() + 1)?
                            }
                            Some(v) => v,
                        };
                    }
                    Insn::Call { callee, this, args, argc } | Insn::CallEval { callee, this, args, argc, .. } => {
                        let level = depth + frames.len() + 1;
                        if level >= STACK_LIMIT {
                            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
                        }
                        let first = r(args);
                        // 快路：被调的是普通 JS 函数（不是类构造器 / 生成器 / async），直接从寄存器开帧
                        let target = regs[r(callee)];
                        if matches!(insn, Insn::Call { .. })
                            && let Some((c, loaded)) = self.plain_closure(target)
                        {
                            let mut next = Self::enter_regs(a, regs, &loaded.func, c, target, regs[r(this)], first, usize::from(argc));
                            if !self.tiered(&loaded, regs, &mut next, acc) {
                                frames.push(core::mem::replace(f, next));
                                continue 'frame;
                            }
                            continue;
                        }
                        let list = regs[first..first + usize::from(argc)].to_vec();
                        match self.resolve(a, regs[r(callee)], regs[r(this)], list, level)? {
                            Callee::Closure { closure, callee, this, args } => {
                                let next = self.enter(a, regs, closure, callee, this, &args);
                                frames.push(core::mem::replace(f, next));
                                continue 'frame;
                            }
                            Callee::Done(v) => *acc = v,
                        }
                    }
                    Insn::PushHandler(target) => f.handlers.push(Handler { target: target.0, env: f.env }),
                    Insn::PopHandler => _ = f.handlers.pop(),
                    Insn::Throw => return Err(a.throw_value(*acc)),
                    Insn::Return => {
                        if !matches!(f.new_target, Value::Undefined) {
                            *acc = self.construct_result(a, f, *acc)?;
                        }
                        match frames.pop() {
                            Some(caller) => {
                                regs.truncate(f.base);
                                *f = caller;
                                continue 'frame;
                            }
                            None => return Ok(Some(Outcome::Return(*acc))),
                        }
                    }
                    Insn::Yield { mode } | Insn::Await { mode } => {
                        debug_assert!(frames.is_empty() && f.base == 0, "a coroutine suspends from its own bottom frame");
                        let saved = Suspended {
                            func: f.func,
                            pc: f.pc,
                            regs: core::mem::take(regs),
                            env: f.env,
                            handlers: f.handlers.drain(..).map(|h| (h.target, h.env)).collect(),
                            args: core::mem::take(&mut f.args),
                            mode: Some(mode.0),
                        };
                        return Ok(Some(Outcome::Suspend { value: *acc, awaiting: matches!(insn, Insn::Await { .. }), saved }));
                    }
                    Insn::GetIterator { dst } => {
                        let rec = a.get_iterator(*acc)?;
                        let d = r(dst);
                        regs[d] = rec.iterator;
                        regs[d + 1] = rec.next;
                        regs[d + 2] = false.into();
                    }
                    Insn::IteratorNext { iter } => {
                        let d = r(iter);
                        *acc = Value::Undefined;
                        if !a.to_boolean(regs[d + 2]) {
                            // 走一步出错也算走完（不再调 `return`）
                            regs[d + 2] = true.into();
                            if let Some(v) = a.iterator_step(IterRecord { iterator: regs[d], next: regs[d + 1] })? {
                                regs[d + 2] = false.into();
                                *acc = v;
                            }
                        }
                    }
                    Insn::IteratorRest { iter } => {
                        let d = r(iter);
                        let mut rest = Vec::new();
                        if !a.to_boolean(regs[d + 2]) {
                            regs[d + 2] = true.into();
                            let rec = IterRecord { iterator: regs[d], next: regs[d + 1] };
                            while let Some(v) = a.iterator_step(rec)? {
                                rest.push(v);
                            }
                        }
                        *acc = a.array(rest).into();
                    }
                    Insn::IteratorClose { iter } => {
                        let d = r(iter);
                        if !a.to_boolean(regs[d + 2]) {
                            regs[d + 2] = true.into();
                            a.iterator_close(regs[d])?;
                        }
                    }
                    Insn::IteratorCloseQuiet { iter } => {
                        let d = r(iter);
                        if !a.to_boolean(regs[d + 2]) {
                            regs[d + 2] = true.into();
                            // 原来的异常优先：`return` 不可调或自己抛错都不管
                            match a.iterator_close(regs[d]) {
                                Ok(()) | Err(Error::Exception(_)) => {}
                                Err(e) => return Err(e),
                            }
                        }
                    }
                    Insn::CreatePrivateName { desc } => *acc = Value::Symbol(a.private_name(func.name_at(desc))),
                    Insn::GetPrivate { obj } => *acc = a.private_get(regs[r(obj)], Self::private_key(*acc))?,
                    Insn::SetPrivate { obj, key } => a.private_set(regs[r(obj)], Self::private_key(regs[r(key)]), *acc)?,
                    Insn::DefinePrivate { obj, key, kind, writable } => {
                        let slot = match (kind, *acc) {
                            (PropertyKind::Getter, Value::Object(f)) => Slot::Accessor { get: Some(f), set: None },
                            (PropertyKind::Setter, Value::Object(f)) => Slot::Accessor { get: None, set: Some(f) },
                            (_, v) => Slot::Data(v),
                        };
                        let prop = Property { slot, enumerable: false, writable, configurable: false };
                        a.private_define(regs[r(obj)], Self::private_key(regs[r(key)]), prop)?;
                    }
                    Insn::HasPrivate { obj } => *acc = a.private_has(regs[r(obj)], Self::private_key(*acc))?.into(),
                    Insn::ToPropertyKey => {
                        if !matches!(*acc, Value::Symbol(_)) {
                            *acc = a.property_key(*acc)?.into();
                        }
                    }
                    Insn::GetAsyncIterator { dst } => {
                        let d = r(dst);
                        let v = *acc;
                        let method = if v.is_nullish() { Value::Undefined } else { a.get_symbol(v, a.realm.symbols.async_iterator, v)? };
                        let (iterator, next, sync) = if method.is_nullish() {
                            let rec = a.get_iterator(v)?;
                            (rec.iterator, rec.next, true)
                        } else {
                            let it = a.call(method, v, &[])?;
                            if !matches!(it, Value::Object(_)) {
                                return Err(a.throw(ErrorKind::Type, "Result of the Symbol.asyncIterator method is not an object"));
                            }
                            let next = a.get_named(it, "next")?;
                            (it, next, false)
                        };
                        regs[d] = iterator;
                        regs[d + 1] = next;
                        regs[d + 2] = false.into();
                        regs[d + 3] = sync.into();
                    }
                    Insn::IteratorCall { iter } => {
                        let d = r(iter);
                        *acc = a.call(regs[d + 1], regs[d], &[])?;
                    }
                    Insn::IteratorResult { iter } => {
                        let d = r(iter);
                        let result = *acc;
                        if !matches!(result, Value::Object(_)) {
                            regs[d + 2] = true.into();
                            let what = a.to_string(result)?;
                            return Err(a.throw(ErrorKind::Type, &alloc::format!("Iterator result {} is not an object", &**what)));
                        }
                        let done = a.get_named(result, "done")?;
                        if a.to_boolean(done) {
                            regs[d + 2] = true.into();
                            *acc = Value::Undefined;
                        } else {
                            *acc = a.get_named(result, "value")?;
                        }
                    }
                    Insn::AsyncIteratorReturn { iter } => {
                        let d = r(iter);
                        *acc = Value::Undefined;
                        if !a.to_boolean(regs[d + 2]) {
                            regs[d + 2] = true.into();
                            let ret = a.get_named(regs[d], "return")?;
                            if !ret.is_nullish() {
                                *acc = a.call(ret, regs[d], &[])?;
                            }
                        }
                    }
                    Insn::LdaNewTarget => *acc = f.new_target,
                    Insn::CreateArguments => *acc = Self::arguments(a, regs, f, func.mode.into(), &func.mapped_params)?,
                    Insn::GetTemplateObject { site, first, count } => {
                        let key = (f.func.0, site);
                        let cached = a.realm.templates.borrow().get(&key).copied();
                        *acc = match cached {
                            Some(o) => o.into(),
                            None => {
                                let (start, n) = (r(first), usize::from(count));
                                let cooked = a.array(regs[start..start + n].to_vec());
                                let raw = a.array(regs[start + n..start + 2 * n].to_vec());
                                let frozen = |v: Value<'gc>| Property { slot: Slot::Data(v), enumerable: false, writable: false, configurable: false };
                                cooked.borrow_mut(a.mc).define("raw", frozen(raw.into()));
                                a.realm.templates.borrow_mut(a.mc).insert(key, cooked);
                                cooked.into()
                            }
                        };
                    }
                    Insn::ForInKeys => {
                        let keys = a.for_in_keys(*acc)?;
                        *acc = a.array(keys).into();
                    }
                    Insn::DelegateStep { iter, mode } => {
                        let d = r(iter);
                        let it = regs[d];
                        let result = match Self::mode(regs[r(mode)]) {
                            Resume::Next => a.call(regs[d + 1], it, &[*acc])?,
                            Resume::Throw => {
                                let th = a.get_named(it, "throw")?;
                                if th.is_nullish() {
                                    a.iterator_close(it)?;
                                    return Err(a.throw(ErrorKind::Type, "The iterator does not provide a 'throw' method"));
                                }
                                a.call(th, it, &[*acc])?
                            }
                            Resume::Return => {
                                let ret = a.get_named(it, "return")?;
                                if ret.is_nullish() { a.iter_result(*acc, true).into() } else { a.call(ret, it, &[*acc])? }
                            }
                        };
                        if !matches!(result, Value::Object(_)) {
                            return Err(a.throw(ErrorKind::Type, "Iterator result is not an object"));
                        }
                        *acc = result;
                    }
                }
            }
        }
    }
}

impl<'gc> Invoke<'gc> for Executor {
    fn call(&self, a: &mut Agent<'gc, '_>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let level = self.depth.get() + 1;
        if level >= STACK_LIMIT {
            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
        }
        // 快路：普通 JS 函数（不是类构造器 / 生成器 / async）直接开帧，实参不先拷成 Vec 过调用解析
        if let Some((c, loaded)) = self.plain_closure(callee) {
            let mut regs = Vec::new();
            let mut f = self.enter(a, &mut regs, c, callee, this, args);
            let mut acc = Value::Undefined;
            if self.tiered(&loaded, &mut regs, &mut f, &mut acc) {
                return Ok(acc);
            }
            return self.run_state(a, State { regs, frames: Vec::new(), f, acc }, level).map(Outcome::returned);
        }
        match self.resolve(a, callee, this, args.to_vec(), level)? {
            Callee::Done(v) => Ok(v),
            Callee::Closure { closure, callee, this, args } => {
                let mut regs = Vec::new();
                let f = self.enter(a, &mut regs, closure, callee, this, &args);
                self.run_frame(a, regs, f, level)
            }
        }
    }

    fn constructs(&self, closure: &Closure<'gc>) -> bool {
        let loaded = self.function(closure.func);
        let f = &loaded.func;
        !f.generator && !f.asynchronous && matches!(f.kind, FunctionKind::Normal | FunctionKind::Constructor { .. })
    }

    fn eval_code(&self, a: &mut Agent<'gc, '_>, src: &str, kind: r8framework::CodeKind) -> Result<Value<'gc>, Error> {
        #[cfg(feature = "compile")]
        {
            let module = match Module::compile(&r8framework::Source { text: src, file: "[eval]" }, kind) {
                Ok(m) => m,
                Err(Error::Syntax(m)) => return Err(a.throw(ErrorKind::Syntax, &m)),
                Err(e) => return Err(e),
            };
            let main = self.load(module);
            let level = self.depth.get() + 1;
            if level >= STACK_LIMIT {
                return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
            }
            let saved = self.depth.replace(level);
            let r = self.run_script(a, main);
            self.depth.set(saved);
            r
        }
        #[cfg(not(feature = "compile"))]
        {
            let _ = (src, kind);
            Err(a.throw(ErrorKind::Syntax, "dynamic code needs the compile feature"))
        }
    }

    fn resume(&self, a: &mut Agent<'gc, '_>, co: JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<Step<'gc>, Error> {
        let level = self.depth.get() + 1;
        if level >= STACK_LIMIT {
            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
        }
        let taken = {
            let mut d = co.borrow_mut(a.mc);
            let ObjectKind::Coroutine(c) = &mut d.kind else { unreachable!("resume is only called on coroutines") };
            match core::mem::replace(&mut c.state, CoroutineState::Running) {
                CoroutineState::Suspended(s) => Ok((s, core::mem::replace(&mut c.fresh, false))),
                other => {
                    c.state = other;
                    Err(matches!(c.state, CoroutineState::Running))
                }
            }
        };
        let (saved, fresh) = match taken {
            Ok(t) => t,
            Err(true) => return Err(a.throw(ErrorKind::Type, "Generator is already running")),
            // 已经跑完
            Err(false) => {
                return match mode {
                    Resume::Next => Ok(Step::Done(Value::Undefined)),
                    Resume::Return => Ok(Step::Done(value)),
                    Resume::Throw => Err(a.throw_value(value)),
                };
            }
        };
        if fresh && mode != Resume::Next {
            Self::set_state(a, co, CoroutineState::Completed);
            return if mode == Resume::Return { Ok(Step::Done(value)) } else { Err(a.throw_value(value)) };
        }
        let mut regs = saved.regs;
        if let Some(m) = saved.mode {
            regs[usize::from(m)] = f64::from(mode).into();
        }
        let handlers = saved.handlers.into_iter().map(|(target, env)| Handler { target, env }).collect();
        let f = Frame { func: saved.func, pc: saved.pc, base: 0, env: saved.env, handlers, new_target: Value::Undefined, new_this: None, args: saved.args };
        match self.run_state(a, State { regs, frames: Vec::new(), f, acc: value }, level) {
            Ok(Outcome::Return(v)) => {
                Self::set_state(a, co, CoroutineState::Completed);
                Ok(Step::Done(v))
            }
            Ok(Outcome::Suspend { value, awaiting, saved }) => {
                Self::set_state(a, co, CoroutineState::Suspended(saved));
                Ok(if awaiting { Step::Await(value) } else { Step::Yield(value) })
            }
            Ok(Outcome::Paused(_)) => unreachable!("coroutine steps run without a budget"),
            Err(e) => {
                Self::set_state(a, co, CoroutineState::Completed);
                Err(e)
            }
        }
    }

    fn construct(&self, a: &mut Agent<'gc, '_>, callee: Value<'gc>, args: &[Value<'gc>], new_target: Value<'gc>) -> Result<Value<'gc>, Error> {
        let level = self.depth.get() + 1;
        if level >= STACK_LIMIT {
            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
        }
        let mut regs = Vec::new();
        match self.prepare(a, &mut regs, callee, args.to_vec(), new_target, level)? {
            Prepared::Done(v) => Ok(v),
            Prepared::Frame(f) => self.run_frame(a, regs, f, level),
        }
    }
}

impl Executor {
    /// 在新的执行状态里跑一帧到返回。
    fn run_frame<'gc>(&self, a: &mut Agent<'gc, '_>, regs: Vec<Value<'gc>>, f: Frame<'gc>, level: usize) -> Result<Value<'gc>, Error> {
        self.run_state(a, State { regs, frames: Vec::new(), f, acc: Value::Undefined }, level).map(Outcome::returned)
    }

    fn run_state<'gc>(&self, a: &mut Agent<'gc, '_>, st: State<'gc>, level: usize) -> Result<Outcome<'gc>, Error> {
        let saved = self.depth.replace(level);
        let r = self.execute(a, st, None);
        self.depth.set(saved);
        r
    }

    /// 把协程的状态换成 `state`。
    fn set_state<'gc>(a: &Agent<'gc, '_>, co: JsObject<'gc>, state: CoroutineState<'gc>) {
        if let ObjectKind::Coroutine(c) = &mut co.borrow_mut(a.mc).kind {
            c.state = state;
        }
    }
}

/// 片与片之间挂在根上的执行状态。
#[cfg(feature = "compile")]
type Parked = r8gc::DynamicRoot<r8gc::Rootable![r8gc::RefLock<Option<State<'_>>>]>;

/// 一片跑完的结果。
#[cfg(feature = "compile")]
enum Slice {
    Done(r8framework::Global),
    Paused(Parked),
}

#[cfg(feature = "compile")]
impl r8framework::Engine for Executor {
    /// 分片跑：每片最多 [`SLICE`] 条指令，停下时执行状态挂到根上，`enter` 返回时回收一段，再进来接着跑。
    fn eval(&mut self, isolate: &mut r8framework::Isolate<'_>, cx: &r8framework::ContextRoot, src: &r8framework::Source<'_>) -> Result<r8framework::Global, Error> {
        let main = self.load(Module::try_from(src)?);
        let this: &Executor = self;
        let mut parked: Option<Parked> = None;
        loop {
            let previous = parked.take();
            let slice = isolate.enter(|scope| -> Result<Slice, Error> {
                let realm = scope.context_of(cx);
                let mut a = scope.agent(realm, this);
                let st = match previous {
                    Some(root) => a.fetch(&root).borrow_mut(a.mc).take().expect("a parked state"),
                    None => this.script_state(&mut a, main),
                };
                match this.execute(&mut a, st, Some(SLICE))? {
                    Outcome::Paused(st) => Ok(Slice::Paused(a.stash(Gc::new(a.mc, r8gc::RefLock::new(Some(*st)))))),
                    Outcome::Return(v) => {
                        a.run_jobs()?;
                        Ok(Slice::Done(a.global(v)))
                    }
                    Outcome::Suspend { .. } => unreachable!("scripts do not suspend"),
                }
            })?;
            match slice {
                Slice::Done(g) => return Ok(g),
                Slice::Paused(root) => parked = Some(root),
            }
        }
    }

    /// 模块不分片：一次进入跑完加载、链接、求值与微任务。
    fn eval_module(&mut self, isolate: &mut r8framework::Isolate<'_>, cx: &r8framework::ContextRoot, src: &r8framework::Source<'_>) -> Result<r8framework::Global, Error> {
        let this: &Executor = self;
        isolate.enter(|scope| {
            let realm = scope.context_of(cx);
            let mut a = scope.agent(realm, this);
            #[cfg(feature = "compile")]
            this.run_module(&mut a, src.file, src.text, 0)?;
            #[cfg(not(feature = "compile"))]
            return Err(src.unsupported(0, "modules"));
            a.run_jobs()?;
            Ok(a.global(Value::Undefined))
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::string::String;
    use alloc::vec::Vec;
    use core::time::Duration;
    use std::sync::Mutex;

    use r8builtins::Builtins;
    use r8framework::{CreateParams, Error, Isolate, Platform, Source, Task};

    use crate::Executor;

    /// 截下 `console.log` 的平台。
    #[derive(Default)]
    struct Capture(Mutex<Vec<String>>);

    impl r8framework::Pool for Capture {
        fn threads(&self) -> usize {
            1
        }

        fn run(&self, tasks: Vec<Task<'_>>) {
            for t in tasks {
                t();
            }
        }
    }

    impl Platform for Capture {
        fn now(&self) -> Duration {
            Duration::ZERO
        }

        fn print(&self, line: &str) {
            self.0.lock().expect("an unpoisoned log").push(String::from(line));
        }
    }

    /// 跑一段脚本：结果（或抛出的值）按 ToString 给出，外加 `console.log` 的输出。
    fn eval(src: &str) -> (Result<String, String>, Vec<String>) {
        let platform = Capture::default();
        let mut isolate = Isolate::new(&platform, CreateParams::default());
        let mut executor = Executor::default();
        let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));
        let r = isolate.run(&mut executor, &cx, &Source { text: src, file: "t.js" });
        let r = isolate.enter(|scope| {
            let realm = scope.context_of(&cx);
            let mut agent = scope.agent(realm, &executor);
            let (ok, v) = match r {
                Ok(g) => (true, agent.local(&g)),
                Err(Error::Exception(g)) => (false, agent.local(&g)),
                Err(e) => return Err(alloc::format!("{e}")),
            };
            let text = agent.to_string(v).map_or_else(|_| String::from("<ToString threw>"), |s| String::from(&**s));
            if ok { Ok(text) } else { Err(text) }
        });
        // 堆借着平台：先拆 isolate 再取日志
        drop(isolate);
        (r, platform.0.into_inner().expect("an unpoisoned log"))
    }

    /// 长循环不停分配临时对象：一次 `eval` 跑完，期间已经完成了回收（分片之间回收）。
    #[test]
    fn collects_while_running() {
        let platform = Capture::default();
        let mut isolate = Isolate::new(&platform, CreateParams::default());
        let mut executor = Executor::default();
        let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));
        let before = isolate.gc_cycles();
        let src = "let s = 0; for (let i = 0; i < 300000; i++) { const o = { v: [i, i] }; s += o.v[0] % 2 } s";
        let r = isolate.run(&mut executor, &cx, &Source { text: src, file: "t.js" });
        assert!(r.is_ok());
        let cycles = isolate.gc_cycles() - before;
        std::println!("gc cycles during one eval: {cycles}");
        assert!(cycles > 0, "no collection finished while the script ran");
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "一张用例表")]
    fn javascript_semantics() {
        let ok = [
            ("1 + 2 * 3", "7"),
            ("'a' + 1", "a1"),
            ("var x = 10; x += 5; x", "15"),
            ("let s = 0; for (let i = 0; i < 10; i++) s += i; s", "45"),
            ("function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); } fib(20)", "6765"),
            ("function mk() { let c = 0; return () => ++c; } const f = mk(); f(); f(); f()", "3"),
            ("const fs = []; for (let i = 0; i < 3; i++) fs[i] = () => i; fs[0]() + fs[1]() + fs[2]()", "3"),
            ("const o = { n: 2, get() { return this.n * 21; } }; o.get()", "42"),
            ("const a = [1, 2, 3]; a[3] = 4; a.length + a[3]", "8"),
            ("`x=${1 + 1}!`", "x=2!"),
            ("function g(x) { switch (x) { case 1: return 'one'; case 2: return 'two'; default: return 'many'; } } g(1) + g(2) + g(5)", "onetwomany"),
            ("let n = 0; outer: for (let i = 0; i < 5; i++) { for (let j = 0; j < 5; j++) { if (j == 3) continue outer; if (i == 3) break outer; n++; } } n", "9"),
            ("typeof undeclared + typeof null + (5 & 3) + (-1 >>> 28)", "undefinedobject115"),
            ("(null ?? 'd') + (0 || 'x') + (1 && 'y')", "dxy"),
            ("let i = 0; do { i++ } while (i < 5); while (i < 8) i++; i", "8"),
            ("const f = function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); }; f(10)", "3628800"),
            ("const o = { v: 7, m() { return (() => this.v)(); } }; o.m()", "7"),
            ("let x = 1; { let x = 2; } x", "1"),
            ("function a() { let x = 1; function b() { function c() { return x + 1; } return c(); } return b(); } a()", "2"),
            ("[1, [2, 3], null] + ''", "1,2,3,"),
            ("'10' == 10 && '10' !== 10 && 2 ** 10 === 1024 && 'b' > 'a'", "true"),
            // 阶段 1：异常
            ("try { throw 1 } catch (e) { e + 1 }", "2"),
            ("let r = ''; try { r += 'a'; throw Error('x') } catch (e) { r += e.message } finally { r += 'f' } r", "axf"),
            ("const log = []; function f() { try { return 'try' } finally { log.push('fin') } } f() + log.join()", "tryfin"),
            ("function f() { try { return 1 } finally { return 2 } } f()", "2"),
            ("let s = ''; for (let i = 0; i < 3; i++) { try { if (i == 1) break; s += i } finally { s += 'f' } } s", "0ff"),
            ("let s = ''; for (let i = 0; i < 3; i++) { try { if (i == 1) continue; s += i } finally { s += 'f' } } s", "0ff2f"),
            ("let s = ''; try { try { throw 'in' } finally { s += 'f' } } catch (e) { s += e } s", "fin"),
            ("try { null.x } catch (e) { e.name }", "TypeError"),
            ("function thrower() { undefinedThing } let m; try { thrower() } catch (e) { m = e.message } m", "undefinedThing is not defined"),
            ("const fs = []; try { throw 5 } catch (e) { fs.push(() => e) } fs[0]()", "5"),
            ("try { [1].map(() => { throw 'cb' }) } catch (e) { e }", "cb"),
            ("String(TypeError('bad'))", "TypeError: bad"),
            // 阶段 1：内建对象
            ("[3, 1, 2].sort().join('-')", "1-2-3"),
            ("[1, 2, 3].map(x => x * 2).filter(x => x > 2).reduce((a, b) => a + b, 0)", "10"),
            ("'Hello'.toUpperCase().split('L')", "HE,,O"),
            ("Math.max(1, 5, 3) + Math.floor(2.7)", "7"),
            ("parseInt('0x1f') + parseFloat('1.5e1abc')", "46"),
            ("Object.keys({ a: 1, b: 2 }).join()", "a,b"),
            ("function f(a, b) { return this.x + a + b } f.bind({ x: 1 }, 2)(3)", "6"),
            ("Math.max.apply(null, [1, 9, 3]) + f2.call({ k: 1 }); function f2() { return this.k }", "10"),
            ("(255).toString(16) + (0.5).toString(2)", "ff0.1"),
            ("[1, 2, 3].includes(2) && 'abc'.includes('b') && [NaN].includes(NaN)", "true"),
            ("({ valueOf() { return 41 } }) + 1", "42"),
            ("`${{ toString() { return 'obj' } }}`", "obj"),
            ("({}) + ''", "[object Object]"),
            ("'abc'.padStart(5, '-') + 'x'.repeat(3) + '  t '.trim()", "--abcxxxt"),
            // 阶段 2：new、类、访问器、instanceof / in / delete
            ("function P(x) { this.x = x } P.prototype.get = function () { return this.x }; new P(5).get()", "5"),
            ("function F() { return { y: 1 } } new F().y", "1"),
            ("class A { constructor(v) { this.v = v } get double() { return this.v * 2 } static make() { return new A(3) } } A.make().double", "6"),
            ("class A { hi() { return 'A' } } class B extends A { hi() { return super.hi() + 'B' } } new B().hi()", "AB"),
            ("class A { constructor(x) { this.x = x } } class B extends A { constructor(x) { super(x * 2); this.y = 1 } } const b = new B(2); b.x + b.y", "5"),
            ("class A { constructor(a, b) { this.s = a + b } } class B extends A {} new B(1, 2).s", "3"),
            ("class C { a = 1; b = this.a + 1; static s = 'st' } const c = new C(); c.a + c.b + C.s", "3st"),
            ("class A { constructor() { this.base = 1 } } class B extends A { f = this.base + 1 } new B().f", "2"),
            ("class A {} class B extends A {} const b = new B(); (b instanceof B) + ',' + (b instanceof A) + ',' + ({} instanceof A)", "true,true,false"),
            ("const o = { a: 1 }; ('a' in o) + ',' + delete o.a + ',' + ('a' in o)", "true,true,false"),
            ("const o = { _v: 1, get v() { return this._v }, set v(x) { this._v = x * 10 } }; o.v = 2; o.v", "20"),
            ("class MyErr extends Error { constructor(m) { super(m); this.name = 'MyErr' } } try { throw new MyErr('boom') } catch (e) { (e instanceof MyErr) + ' ' + (e instanceof Error) + ' ' + e }", "true true MyErr: boom"),
            ("new TypeError('x').message + (new Error() instanceof Error)", "xtrue"),
            ("const K = class Inner { who() { return Inner.tag } }; K.tag = 'k'; new K().who()", "k"),
            ("class A { v() { return 1 } } class B extends A { v() { return (() => super.v() + 1)() } } new B().v()", "2"),
            ("const o = {}; Object.defineProperty(o, 'x', { get() { return 7 } }); o.x + Object.keys(o).length", "7"),
            ("class A { m() {} } Object.keys(new A()).length + Object.keys({ p: 1 }).length", "1"),
            ("const o = Object.freeze({ a: 1 }); o.a = 2; o.a", "1"),
            // 阶段 3：剩余 / 默认参数、展开、解构、可选链
            ("function f(a, b = a + 1, ...rest) { return a + b + rest.length } f(1) + f(1, 5, 9, 9)", "11"),
            ("const [x, , y = 3, ...z] = [1, 2, undefined, 4, 5]; x + y + z.join('')", "445"),
            ("const { a, b: { c }, d = 4, ...rest } = { a: 1, b: { c: 2 }, e: 5, f: 6 }; a + c + d + Object.keys(rest).join()", "7e,f"),
            ("let p = 1, q = 2; [p, q] = [q, p]; p * 10 + q", "21"),
            ("let m, n; ({ m, n = 7 } = { m: 3 }); m + n", "10"),
            ("function g({ x, y } = {}, [z] = [9]) { return (x ?? 0) + (y ?? 0) + z } g() + g({ x: 1, y: 2 }, [3])", "15"),
            ("Math.max(...[1, 5, 3], 2)", "5"),
            ("[0, ...[1, 2], ...'ab', , 3].join()", "0,1,2,a,b,,3"),
            ("const o = { a: 1, ...{ b: 2, a: 3 }, c: 4 }; o.a + o.b + o.c", "9"),
            ("class P { constructor(a, b) { this.s = a + b } } new P(...[2, 3]).s", "5"),
            ("class A { constructor(...xs) { this.n = xs.length } } class B extends A { constructor(...xs) { super(...xs, 0) } } new B(1, 2).n", "3"),
            ("const o = { a: { b: 1 }, f() { return this.a.b } }; [o?.a?.b, o.x?.y, o.f?.(), o.g?.(), o?.['a']?.b].join()", "1,,1,,1"),
            ("null?.x.y.z", "undefined"),
            ("function h({ k }) { return () => k } h({ k: 'c' })()", "c"),
            ("try { throw { code: 7 } } catch ({ code }) { code }", "7"),
            ("function d(a, b = () => a) { a = 2; return b() } d(1)", "2"),
            ("const { length } = 'abc'; const [first] = 'xyz'; length + first", "3x"),
            ("var it = {}; let closed = 0; it[Symbol.iterator] = function () { return { next() { return { done: false } }, return() { closed++; return {} } } }; function t() { throw 'lhs' } let r; try { 0, [...{}[t()]] = it } catch (e) { r = e } r + closed", "lhs1"),
            ("const log = []; const it = { [Symbol.iterator]() { return { next() { log.push('next'); return { done: false, value: 1 } }, return() { log.push('ret'); return {} } } } }; const o = { get p() { log.push('ref'); return {} } }; [o.p.x] = it; log.join()", "ref,next,ret"),
            ("let c = 0; const it = { [Symbol.iterator]() { return { next: () => ({ done: false, value: undefined }), return() { c++; return {} } } } }; try { const [a = (() => { throw 1 })()] = it } catch {} c", "1"),
            // 阶段 4：Symbol、迭代、for-of / for-in、生成器
            ("const s = Symbol('d'); const o = { [s]: 1 }; o[s] + typeof s + s.description + String(s)", "1symboldSymbol(d)"),
            ("const o = { [Symbol.toPrimitive](h) { return h === 'number' ? 42 : 'str' } }; +o + ',' + `${o}`", "42,str"),
            ("const it = { [Symbol.iterator]() { let i = 0; return { next: () => ({ value: i, done: i++ >= 3 }) } } }; [...it].join()", "0,1,2"),
            ("let s = 0; for (const x of [1, 2, 3]) s += x; s", "6"),
            ("let r = ''; for (const c of 'héllo') r = c + r; r", "olléh"),
            ("const fs = []; for (const x of [1, 2]) fs.push(() => x); fs[0]() + fs[1]()", "3"),
            ("let closed = 0; const it = { [Symbol.iterator]() { return { next: () => ({ value: 1, done: false }), return() { closed++; return {} } } } }; for (const x of it) break; for (const x of it) { try { throw 0 } catch {} break } function f() { for (const x of it) return 1 } f(); closed", "3"),
            ("let s = ''; for (const [k, v] of Object.entries({ a: 1, b: 2 })) s += k + v; s", "a1b2"),
            ("const o = { a: 1, b: 2 }; let s = ''; for (const k in o) s += k; for (const i in ['x', 'y']) s += i; s", "ab01"),
            ("function P() { this.own = 1 } P.prototype.inh = 2; let s = ''; for (const k in new P()) s += k; s", "owninh"),
            ("let s = ''; outer: for (const a of [1, 2]) { for (const b of [1, 2]) { if (b == 2) continue outer; s += a + '' + b } } s", "1121"),
            ("function* g() { yield 1; yield 2; return 3 } const it = g(); [it.next().value, it.next().value, it.next().value, it.next().done].join()", "1,2,3,true"),
            ("function* g() { let i = 0; while (i < 3) yield i++ } [...g()].join()", "0,1,2"),
            ("function* g() { const x = yield 1; yield x * 2 } const it = g(); it.next(); it.next(21).value", "42"),
            ("const log = []; function* g() { try { yield 1; yield 2 } finally { log.push('f') } } const it = g(); it.next(); [it.return(9).value, it.next().done, log.join()].join()", "9,true,f"),
            ("function* g() { try { yield 1 } catch (e) { yield 'caught ' + e } } const it = g(); it.next(); it.throw('x').value", "caught x"),
            ("function* inner() { yield 1; yield 2; return 'r' } function* outer() { const v = yield* inner(); yield v } [...outer()].join()", "1,2,r"),
            ("function* nat() { let n = 0; while (true) yield n++ } const [a, b, c] = nat(); a + b + c", "3"),
            ("const o = { *[Symbol.iterator]() { yield 'a'; yield 'b' } }; [...o].join('')", "ab"),
            ("class C { *gen() { yield this.v } } const c = new C(); c.v = 5; c.gen().next().value", "5"),
            ("[...[1, 2].entries()].join(';') + [...[7].keys()]", "0,1;1,20"),
            // 阶段 5：正则、BigInt
            ("const m = /a(b)c/.exec('xabcx'); m.index + m[1] + m", "1babc,b"),
            ("/^\\d+$/.test('123') && !/^\\d+$/.test('12a')", "true"),
            ("'a1b22c333'.match(/\\d+/g).join()", "1,22,333"),
            ("'2024-11-11'.replace(/(\\d+)-(\\d+)-(\\d+)/, '$3/$2/$1')", "11/11/2024"),
            ("'aaa'.replace(/a/g, (m, i) => i)", "012"),
            ("'a,b;c'.split(/[,;]/).join('|') + ' ' + 'abc'.split(/(b)/).join('|')", "a|b|c a|b|c"),
            ("const { groups: { y, m } } = /(?<y>\\d{4})-(?<m>\\d{2})/.exec('2024-11'); y + m", "202411"),
            ("const re = /o/g; let n = 0; while (re.exec('foo boo')) n++; n", "4"),
            ("/x/gi.flags + /x/gi + new RegExp('a+', 'y').sticky", "gi/x/gitrue"),
            ("'hello'.search(/l+/) + [...'a1b2'.matchAll(/[a-z](\\d)/g)].map(m => m[1]).join()", "21,2"),
            ("'a.b.c'.replaceAll('.', '-') + 'x'.replace('x', '[$&]')", "a-b-c[x]"),
            ("/É/i.test('é')", "true"),
            ("2n ** 64n", "18446744073709551616"),
            ("typeof 1n + (10n / 3n) + (-7n % 2n)", "bigint3-1"),
            ("(1n << 100n) >> 99n", "2"),
            ("10n > 9 && 10n == 10 && 10n !== 10 && 1n < 1.5 && 2n > '1'", "true"),
            ("let x = 5n; x++; x * 2n", "12"),
            ("BigInt('0x10') + BigInt(3) + BigInt(true)", "20"),
            ("(255n).toString(16) + String(-1n)", "ff-1"),
            ("Number(2n ** 53n) === 2 ** 53", "true"),
            ("-(2n) + ~5n", "-8"),
            // 阶段 6：暂时性死区、脚本作用域
            ("let s = 0; for (let i = 0; i < 3; i++) { let t = i; s += t } s", "3"),
            ("function f() { let x = 1; { const y = x + 1; return y } } f()", "2"),
            ("let lx = 1; var vx = 2; (globalThis.lx === undefined) + ',' + globalThis.vx + ',' + lx", "true,2,1"),
            ("class A {} class B extends A { constructor() { super(); this.x = 1 } } new B().x", "1"),
            ("'a'", "a"),
            ("'use strict'; 'b'", "b"),
            // 类的私有名、静态块、计算字段名
            ("class C { #x = 1; #m() { return this.#x + 1 } get #g() { return this.#x * 10 } set #g(v) { this.#x = v } run() { this.#g = 5; return [this.#m(), this.#g, #x in this, #x in {}].join() } } new C().run()", "6,50,true,false"),
            ("class A { static #count = 0; static inc() { return ++A.#count } static { A.inc(); A.inc() } static get n() { return A.#count } } A.n", "2"),
            ("let k = 'dyn'; class C { [k + 1] = 5; static [k] = 6 } [new C().dyn1, C.dyn].join()", "5,6"),
            ("class C { #m() { return 1 } same(o) { return this.#m === o.#m } } new C().same(new C())", "true"),
            ("class B { #p = 2; static has(o) { return #p in o } } class D extends B { #q = 3; q() { return this.#q } } const o = new D(); [B.has(o), o.q()].join()", "true,3"),
            ("class C { #a = 1; inc() { this.#a += 2; this.#a++; return this.#a } } new C().inc()", "4"),
            ("class C { static { var v = 1; this.v = v } } C.v + typeof v", "1undefined"),
            ("let i = 0; class C { [i++] = 'a'; [i++] = 'b' } const c1 = new C(), c2 = new C(); c1[0] + c1[1] + c2[0] + i", "aba2"),
            // 函数的 name / length、new.target、标签模板、arguments
            ("function f(a, b = 1, c) {} const g = () => 0; let h = function () {}; const o = { m() {}, get x() { return 1 }, k: () => 0 }; class C { static s() {} } [f.name, f.length, g.name, h.name, o.m.name, Object.getOwnPropertyDescriptor(o, 'x').get.name, o.k.name, C.name, C.s.name, (class {}).name, [].map.name, f.bind(null, 1).name, f.bind(null, 1).length].join()", "f,1,g,h,m,get x,k,C,s,,map,bound f,0"),
            ("let x; x = function () {}; const { y = () => 0 } = {}; [x.name, y.name, (function (z = class {}) { return z.name })()].join()", "x,y,z"),
            ("function F() { this.t = new.target === F } function G() { return new.target } [new F().t, G() === undefined, (() => { function H() { this.t = (() => new.target)() === H } return new H().t })()].join()", "true,true,true"),
            ("function tag(s, ...v) { return s.raw.join('|') + ':' + s.join('|') + ':' + v.join() } tag`a${1}b${2}c`", "a|b|c:a|b|c:1,2"),
            ("function t(s) { return s } const sites = []; for (let i = 0; i < 2; i++) sites.push(t`x`); sites[0] === sites[1] && t`x` !== sites[0]", "true"),
            (r"function t(s) { return s.raw[0] + (s[0] === undefined) } t`\unicode`", r"\unicodetrue"),
            ("const o = { f(s) { return this.k + s[0] }, k: 'k' }; o.f`!`", "k!"),
            ("function f() { return arguments.length + ':' + [...arguments].join() + ':' + arguments[1] } f(1, 2, 3)", "3:1,2,3:2"),
            ("function f() { const g = () => arguments[0]; return g() } f('lex')", "lex"),
            ("function f(arguments) { return arguments } f(5)", "5"),
            ("function f() { return Object.keys(arguments).join() + (arguments.callee === f) } f('a', 'b')", "0,1true"),
            // Map / Set / WeakMap / WeakSet
            ("const m = new Map([[1, 'a'], ['1', 'b']]); m.set(NaN, 'n'); [m.get(1), m.get('1'), m.get(NaN), m.size, m.has(2)].join()", "a,b,n,3,false"),
            ("1 / [...new Map([[-0, 1]]).keys()][0]", "Infinity"),
            ("const s = new Set([1, 2, 3]); const out = []; for (const x of s) { out.push(x); if (x == 1) { s.delete(2); s.add(4) } } out.join()", "1,3,4"),
            ("const m = new Map([['a', 1], ['b', 2]]); let r = ''; m.forEach((v, k) => r += k + v); r + [...m].join(';')", "a1b2a,1;b,2"),
            ("[...new Set('hello')].join('')", "helo"),
            ("const k = {}; const m = new Map([[k, 1]]); m.get(k) + ',' + m.get({})", "1,undefined"),
            ("const s = new Set([1, 2]); s.clear(); s.size + ',' + s.has(1)", "0,false"),
            ("const wm = new WeakMap(); const o = {}; wm.set(o, 5); `${wm.get(o)},${wm.has({})},${new WeakSet([o]).has(o)}`", "5,false,true"),
            ("class M extends Map { constructor() { super([[1, 2]]) } } const x = new M(); x.get(1) + ',' + (x instanceof Map) + ',' + (x instanceof M)", "2,true,true"),
            // JSON
            (r"JSON.stringify({ a: 1, b: [true, null, 'x'], c: undefined, d: () => 1 })", r#"{"a":1,"b":[true,null,"x"]}"#),
            (r"JSON.stringify([undefined, NaN, function () {}])", r"[null,null,null]"),
            (r#"JSON.stringify('a"b\n')"#, r#""a\"b\n""#),
            (r"JSON.stringify({ a: [1, 2], b: {} }, null, 2)", "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {}\n}"),
            (r"JSON.stringify({ a: 1, b: 2 }, (k, v) => k === 'b' ? undefined : v)", r#"{"a":1}"#),
            (r"JSON.stringify({ a: 1, b: 2, c: 3 }, ['c', 'a'])", r#"{"c":3,"a":1}"#),
            (r"JSON.stringify({ toJSON() { return 'x' } })", r#""x""#),
            (r#"const o = JSON.parse('{"a": [1, 2.5e1, "s\\u0041"], "b": null}'); o.a[1] + o.a[2] + o.b"#, "25sAnull"),
            (r"JSON.parse('[1, 2, 3]', (k, v) => typeof v === 'number' ? v * 2 : v).join()", "2,4,6"),
            (r#"JSON.stringify(JSON.parse('{"x":{"y":[1,{"z":"w"}]}}'))"#, r#"{"x":{"y":[1,{"z":"w"}]}}"#),            // 动态代码与严格模式
            ("new Function('a', 'b', 'return a + b')(2, 3)", "5"),
            ("Function('return typeof this')()", "object"),
            ("var gv = 1; eval('gv + 1')", "2"),
            ("eval('var ev = 4'); ev", "4"),
            ("const GF = Object.getPrototypeOf(function* () {}).constructor; GF('yield 1; yield 2')().next().value", "1"),
            ("(function () { return this === globalThis })()", "true"),
            ("(function () { 'use strict'; return typeof this })()", "undefined"),
            ("(function () { return typeof this }).call(1)", "object"),
            ("const o = Object.freeze({ a: 1 }); o.a = 2; o.a", "1"),
            ("class C { m() { return typeof this } } const m = new C().m; m()", "undefined"),
            // 属性模型与 Object / Array
            ("Object.keys({ b: 1, 2: 1, a: 1, 1: 1 }).join()", "1,2,b,a"),
            ("Object.getOwnPropertyNames([1, 2]).join()", "0,1,length"),
            ("const a = [1, 2, 3]; delete a[1]; (1 in a) + ',' + a.length", "false,3"),
            ("const f = Object.freeze([1]); let e; try { f.push(2) } catch (x) { e = x.name } Object.isFrozen(f) + e + f.length", "trueTypeError1"),
            ("Array.prototype.map.call({ length: 2, 0: 'a', 1: 'b' }, x => x + x).join()", "aa,bb"),
            ("[1, [2, [3, [4]]]].flat(Infinity).join() + '|' + [3, 1, 2].toSorted().join() + '|' + [1, 2, 3].with(1, 9).join()", "1,2,3,4|1,2,3|1,9,3"),
            ("({ __proto__: Array.prototype }) instanceof Array", "true"),
            ("Object.fromEntries([['a', 1], ['b', 2]]).b + Object.entries({ x: 3 })[0][1]", "5"),
            ("class M extends Map {} const m = new M(); (m instanceof M) + ',' + m.size", "true,0"),
            ("Math.max.length + [].push.length + Object.defineProperty.length + typeof new Number(1)", "6object"),
            ("const a = [1, 2, 3]; a.length = 1; a.splice(0, 0, 'x', 'y'); a.join() + Array.from({ length: 2 }, (_, i) => i * 2).join()", "x,y,10,2"),
            // Proxy / Reflect / Iterator 助手 / Set 方法
            ("const log = []; const p = new Proxy({ a: 1 }, { get(t, k, r) { log.push(k); return Reflect.get(t, k, r) } }); p.a + log.join()", "1a"),
            ("const f = new Proxy(function (x) { return x * 2 }, { apply(t, th, args) { return t(...args) + 1 } }); f(5) + typeof f", "11function"),
            ("const { proxy, revoke } = Proxy.revocable({}, {}); revoke(); let e; try { proxy.x } catch (x) { e = x.name } e", "TypeError"),
            ("function* g() { yield 1; yield 2; yield 3; yield 4 } g().map(x => x * 10).filter(x => x > 10).take(2).toArray().join() + '|' + g().drop(2).reduce((a, b) => a + b)", "20,30|7"),
            ("new Set([1, 2, 3]).union(new Set([3, 4])).size + [...new Set([1, 2, 3]).difference(new Set([2]))].join()", "41,3"),
            ("Map.groupBy([1, 2, 3], x => x % 2).get(1).join() + Reflect.ownKeys({ b: 1, 1: 0 }).join()", "1,31,b"),
            ("const o = Object.preventExtensions({ a: 1 }); o.b = 2; Object.isExtensible(o) + ',' + ('b' in o) + ',' + Object.isSealed(Object.seal({}))", "false,false,true"),
        ];
        for (src, want) in ok {
            assert_eq!(eval(src).0, Ok(String::from(want)), "{src}");
        }
        let thrown = [
            ("undefinedVar", "ReferenceError: undefinedVar is not defined"),
            ("function r() { return r(); } r()", "RangeError: Maximum call stack size exceeded"),
            ("const c = 1; c = 2", "TypeError: Assignment to constant variable."),
            ("throw 'boom'", "boom"),
            ("let u; u.x", "TypeError: Cannot read properties of undefined (reading 'x')"),
            ("try { throw 1 } finally { }", "1"),
            ("(1)()", "TypeError: 1 is not a function"),
            ("class A {} A()", "TypeError: Class constructor A cannot be invoked without 'new'"),
            ("class A {} class B extends A { constructor() {} } new B()", "ReferenceError: Must call super constructor in derived class before accessing 'this' or returning from derived constructor"),
            ("class A extends 3 {}", "TypeError: Class extends value 3 is not a constructor or null"),
            ("1 in 2", "TypeError: Cannot use 'in' operator to search for '1' in 2"),
            ("const { x } = null", "TypeError: Cannot read properties of null (reading 'x')"),
            ("[...1]", "TypeError: 1 is not iterable"),
            ("for (const x of 5) {}", "TypeError: 5 is not iterable"),
            ("const it = (function* () { yield it.next() })(); it.next()", "TypeError: Generator is already running"),
            ("1n + 1", "TypeError: Cannot mix BigInt and other types, use explicit conversions"),
            ("1n / 0n", "RangeError: Division by zero"),
            ("BigInt(1.5)", "RangeError: The number 1.5 cannot be converted to a BigInt because it is not an integer"),
            ("+1n", "TypeError: Cannot convert a BigInt value to a number"),
            ("1n >>> 1n", "TypeError: BigInts have no unsigned right shift, use >> instead"),
            ("new RegExp('a', 'gg')", "SyntaxError: Invalid flags supplied to RegExp constructor 'gg'"),
            ("{ x; let x = 1 }", "ReferenceError: Cannot access 'x' before initialization"),
            ("let y = y", "ReferenceError: Cannot access 'y' before initialization"),
            ("function f() { return g() } const v = f(); function g() { return w } let w = 1", "ReferenceError: Cannot access 'w' before initialization"),
            ("typeof z; let z", "ReferenceError: Cannot access 'z' before initialization"),
            ("class A {} class B extends A { constructor() { this.x = 1; super() } } new B()", "ReferenceError: Must call super constructor in derived class before accessing 'this' or returning from derived constructor"),
            ("function f() { const g = () => q; g(); let q = 1 } f()", "ReferenceError: Cannot access 'q' before initialization"),
            ("switch (1) { case 0: let a = 1; case 1: a }", "ReferenceError: Cannot access 'a' before initialization"),
            ("function f() { const k = 1; k = 2 } f()", "TypeError: Assignment to constant variable."),
            ("function f() { m = 1; let m } f()", "ReferenceError: Cannot access 'm' before initialization"),
            (r"JSON.parse('{a: 1}')", "SyntaxError: Unexpected token 'a' in JSON at position 1 (expected a property name)"),
            (r"JSON.parse('[1,]')", "SyntaxError: Unexpected token ']' in JSON at position 3 (expected a value)"),
            ("const o = {}; o.self = o; JSON.stringify(o)", "TypeError: Converting circular structure to JSON"),
            ("JSON.stringify(1n)", "TypeError: Do not know how to serialize a BigInt"),
            ("new WeakMap().set(1, 1)", "TypeError: Invalid value used as weak map key"),
            ("class C { #x = 1; static read(o) { return o.#x } } C.read({})", "TypeError: Cannot read private member #x from an object whose class did not declare it"),
            ("class C { #m() {} f() { this.#m = 1 } } new C().f()", "TypeError: Private method #m is not writable"),
            ("class C { get #g() { return 1 } f() { this.#g = 2 } } new C().f()", "TypeError: '#g' was defined without a setter"),
            ("class C { #x; static f(o) { return #x in o } } C.f(1)", "TypeError: Cannot use 'in' operator to search for '#x' in 1"),            ("'use strict'; undeclaredStrict = 1", "ReferenceError: undeclaredStrict is not defined"),
            ("'use strict'; const o = Object.freeze({ a: 1 }); o.a = 2", "TypeError: Cannot assign to read only property 'a'"),
            ("(function () { 'use strict'; return arguments.callee })()", "TypeError: 'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions or the arguments objects for calls to them"),
            ("new Math.max()", "TypeError: max is not a constructor"),
            ("Map()", "TypeError: Constructor Map requires 'new'"),
            ("'use strict'; Object.preventExtensions([]).push(1)", "TypeError: Cannot add property 0, object is not extensible"),
            ("eval('let let = 1')", "SyntaxError: [eval]: `let` cannot be declared as a variable name inside of a `let` declaration"),
        ];
        for (src, want) in thrown {
            assert_eq!(eval(src).0, Err(String::from(want)), "{src}");
        }
        let (r, out) = eval("console.log('a', 1, [2, 3]); console.log(); 0");
        assert_eq!((r, out), (Ok(String::from("0")), alloc::vec![String::from("a 1 2,3"), String::new()]));
        // 阶段 4：Promise 与 async（结果看微任务跑完后的输出顺序）
        let printed = [
            ("Promise.resolve(1).then(v => console.log('then', v)); console.log('sync')", "sync|then 1"),
            ("async function f() { const x = await Promise.resolve(2); return x * 3 } f().then(v => console.log(v)); console.log('first')", "first|6"),
            ("async function f() { throw new Error('bad') } f().catch(e => console.log(e.message))", "bad"),
            ("async function f() { try { await Promise.reject('no') } catch (e) { return 'caught ' + e } } f().then(console.log)", "caught no"),
            ("Promise.all([1, Promise.resolve(2), new Promise(r => r(3))]).then(v => console.log(v.join()))", "1,2,3"),
            ("Promise.resolve().then(() => console.log('a')).then(() => console.log('c')); Promise.resolve().then(() => console.log('b'))", "a|b|c"),
            ("const f = async x => x + 1; f(1).then(console.log)", "2"),
            ("Promise.resolve({ then(r) { r(7) } }).then(console.log)", "7"),
            ("Promise.reject(1).finally(() => console.log('fin')).catch(e => console.log('e', e))", "fin|e 1"),
            ("async function a() { console.log('a1'); await 0; console.log('a2') } a(); console.log('main')", "a1|main|a2"),
            ("class K { async m() { return this.v } } const k = new K(); k.v = 'mv'; k.m().then(console.log)", "mv"),
            // async 生成器、for await
            ("async function* g() { yield 1; const x = yield 2; yield x * 10; return 'end' } (async () => { const it = g(); const r = []; r.push((await it.next()).value); r.push((await it.next()).value); r.push((await it.next(4)).value); const last = await it.next(); r.push(last.value, last.done); console.log(r.join()) })()", "1,2,40,end,true"),
            ("async function* nums() { for (let i = 0; i < 3; i++) { await null; yield i } } (async () => { let s = ''; for await (const n of nums()) s += n; for await (const v of [Promise.resolve('a'), 'b']) s += v; console.log(s) })()", "012ab"),
            ("async function* g() { try { yield 1; yield 2 } finally { console.log('cleanup') } } (async () => { for await (const x of g()) { console.log(x); break } console.log('after') })()", "1|cleanup|after"),
            ("const it = (async function* () { yield 1 })(); Promise.all([it.next(), it.next(), it.next()]).then(rs => console.log(rs.map(r => r.value + ':' + r.done).join()))", "1:false,undefined:true,undefined:true"),
            ("async function* inner() { yield 1; await null; yield 2; return 'r' } async function* outer() { const v = yield* inner(); yield v; yield* [Promise.resolve(3), 4] } (async () => { const out = []; for await (const x of outer()) out.push(x); console.log(out.join()) })()", "1,2,r,3,4"),
            ("async function* inner() { try { yield 1; yield 2 } finally { console.log('inner fin') } } async function* outer() { yield* inner() } (async () => { const it = outer(); await it.next(); const r = await it.return(7); console.log(r.value, r.done) })()", "inner fin|7 true"),
            ("async function* g() { throw new Error('boom') } g().next().catch(e => console.log(e.message))", "boom"),
        ];
        for (src, want) in printed {
            let (r, out) = eval(src);
            assert!(r.is_ok(), "{src}: {r:?}");
            assert_eq!(out.join("|"), want, "{src}");
        }
    }
}
