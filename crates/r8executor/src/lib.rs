//! r8executor：r8bytecode 字节码执行器（`no_std`）。
//!
//! 显式帧栈（不靠宿主递归），所有帧共用一个寄存器文件；被捕获的绑定在堆上的 [`Env`] 链里。
//! 异常：每帧一个动态处理器栈（`PushHandler` / `PopHandler`），抛出时弹本帧处理器，没有就展开到调用者。
//! 一个 `Executor` 只配一个 isolate：它装着已加载的函数，堆里的闭包按 [`FuncId`] 指回来。
#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use core::cell::Cell;

use r8bytecode::{Constant, Function, FunctionKind, Insn, Label, Module, PropertyKind, Reg};
use gc_arena::Gc;
use r8frame::{Agent, Closure, Coroutine, CoroutineState, Env, Error, ErrorKind, FuncId, Invoke, IterRecord, JsEnv, JsObject, ObjectData, ObjectKind, Property, Resume, Slot, Step, Suspended, Value};

/// 调用深度上限（含宿主函数里重入的 JS），过了抛 RangeError
const STACK_LIMIT: usize = 10_000;

/// 一个已加载的函数：本体加它所在模块的第一个函数编号（`CreateClosure` 的下标相对它）。
struct Loaded {
    base: u32,
    func: Function,
}

/// 一个异常处理器：落点与装的时候的环境。
struct Handler<'gc> {
    target: Label,
    env: Option<JsEnv<'gc>>,
}

/// 一个调用帧。
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

/// 一次执行的全部状态。
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
                self.f.pc = h.target.0 as usize;
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
}

impl<'gc> Outcome<'gc> {
    /// 普通函数只会返回（`yield` / `await` 只在协程自己的那一帧里出现）。
    fn returned(self) -> Value<'gc> {
        match self {
            Outcome::Return(v) => v,
            Outcome::Suspend { .. } => unreachable!("only coroutines suspend"),
        }
    }
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

/// 字节码执行器。
#[derive(Default)]
pub struct Executor {
    code: Vec<Loaded>,
    /// 当前调用深度（各层重入的 `execute` 加起来）
    depth: Cell<usize>,
}

impl Executor {
    /// 装进一个模块，返回脚本本身的函数编号。
    pub fn load(&mut self, module: Module) -> FuncId {
        let base = u32::try_from(self.code.len()).expect("fewer than 2^32 functions");
        self.code.extend(module.functions.into_iter().map(|func| Loaded { base, func }));
        FuncId(base + Module::MAIN)
    }

    fn function(&self, f: FuncId) -> &Loaded {
        &self.code[f.0 as usize]
    }

    /// 跑脚本 `main`（`this` 是全局对象）。
    pub fn run_script<'gc>(&self, a: &mut Agent<'gc, '_>, main: FuncId) -> Result<Value<'gc>, Error> {
        let this = Value::Object(a.realm.global);
        let mut regs = Vec::new();
        let f = self.enter(a, &mut regs, Closure::new(main, None), Value::Undefined, this, &[]);
        self.execute(a, State { regs, frames: Vec::new(), f, acc: Value::Undefined }).map(Outcome::returned)
    }

    /// 协程函数被调：生成器给出还没跑的生成器对象；async 函数跑到第一个 `await`，给出它的 Promise。
    fn start<'gc>(&self, a: &mut Agent<'gc, '_>, closure: Closure<'gc>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>], depth: usize) -> Result<Value<'gc>, Error> {
        let mut regs = Vec::new();
        let f = self.enter(a, &mut regs, closure, callee, this, args);
        let saved = Suspended { func: f.func, pc: 0, regs, env: f.env, handlers: Vec::new(), args: f.args, mode: None };
        let generator = self.function(closure.func).func.generator;
        let proto = if generator {
            match a.get_named(callee, "prototype")? {
                Value::Object(p) => p,
                _ => a.realm.generator_proto,
            }
        } else {
            a.realm.object_proto
        };
        let promise = (!generator).then(|| a.new_promise());
        let co = ObjectData { kind: ObjectKind::Coroutine(Coroutine { state: CoroutineState::Suspended(saved), fresh: true, promise }), proto: Some(proto), ..ObjectData::default() }.alloc(a.mc);
        let Some(p) = promise else { return Ok(co.into()) };
        let outer = self.depth.replace(depth);
        let r = a.drive(co, Resume::Next, Value::Undefined);
        self.depth.set(outer);
        r?;
        Ok(p.into())
    }

    /// 解开绑定函数、直接调宿主函数；剩下 JS 闭包交给调用者进帧。
    fn resolve<'gc>(&self, a: &mut Agent<'gc, '_>, mut callee: Value<'gc>, mut this: Value<'gc>, mut args: Vec<Value<'gc>>, depth: usize) -> Result<Callee<'gc>, Error> {
        while let Value::Object(o) = callee {
            let kind = match &o.borrow().kind {
                ObjectKind::Function(c) if matches!(self.function(c.func).func.kind, FunctionKind::Constructor { .. }) => {
                    let name = &self.function(c.func).func.name;
                    return Err(a.throw(ErrorKind::Type, &alloc::format!("Class constructor {name} cannot be invoked without 'new'")));
                }
                ObjectKind::Function(c) if { let f = &self.function(c.func).func; f.generator || f.asynchronous } => {
                    let c = *c;
                    return self.start(a, c, callee, this, &args, depth).map(Callee::Done);
                }
                ObjectKind::Function(c) => return Ok(Callee::Closure { closure: *c, callee, this, args }),
                ObjectKind::Resolver { .. } => return a.run_resolver(o, args.first().copied().unwrap_or_default()).map(Callee::Done),
                ObjectKind::Native(n) => Ok(*n),
                ObjectKind::Bound(b) => Err((b.target, b.this, b.args.clone())),
                _ => break,
            };
            match kind {
                Ok(n) => {
                    let saved = self.depth.replace(depth);
                    let r = (n.0)(a, this, &args);
                    self.depth.set(saved);
                    return r.map(Callee::Done);
                }
                Err((target, bound_this, mut bound_args)) => {
                    bound_args.extend(args);
                    (callee, this, args) = (target, bound_this, bound_args);
                }
            }
        }
        let what = a.to_string(callee)?;
        Err(a.throw(ErrorKind::Type, &alloc::format!("{} is not a function", &**what)))
    }

    /// 建一帧：在寄存器文件末尾开出本函数的寄存器，按调用约定放 `this`、自身、实参。
    fn enter<'gc>(&self, a: &mut Agent<'gc, '_>, regs: &mut Vec<Value<'gc>>, closure: Closure<'gc>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Frame<'gc> {
        a.profiler.enter(closure.func);
        let f = &self.function(closure.func).func;
        let base = regs.len();
        regs.resize(base + usize::from(f.regs), Value::Undefined);
        regs[base + usize::from(Reg::THIS.0)] = this;
        regs[base + usize::from(Reg::CALLEE.0)] = callee;
        let params = usize::from(f.params).min(args.len());
        let first = base + usize::from(Reg::PARAMS);
        regs[first..first + params].copy_from_slice(&args[..params]);
        let args = if f.keeps_args { args.to_vec() } else { Vec::new() };
        Frame { func: closure.func, pc: 0, base, env: closure.env, handlers: Vec::new(), new_target: Value::Undefined, new_this: None, args }
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
                ObjectKind::Bound(b) => Err((b.target, b.args.clone())),
                _ => break,
            };
            match kind {
                Ok(Ok(closure)) => {
                    let func = &self.function(closure.func).func;
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
                    let mut f = self.enter(a, regs, closure, callee, this.unwrap_or_default(), &args);
                    f.new_target = new_target;
                    f.new_this = this;
                    return Ok(Prepared::Frame(f));
                }
                Ok(Err(native)) => {
                    let saved = self.depth.replace(depth);
                    let r = (native.0)(a, Value::Undefined, &args);
                    self.depth.set(saved);
                    let obj = match r? {
                        Value::Object(made) => made,
                        _ => a.object(),
                    };
                    // `class X extends Error`：宿主构造器建的对象换成派生类的原型
                    if !matches!(new_target, Value::Object(t) if Gc::ptr_eq(t, o))
                        && let Value::Object(p) = a.get_named(new_target, "prototype")?
                    {
                        obj.borrow_mut(a.mc).proto = Some(p);
                    }
                    return Ok(Prepared::Done(obj.into()));
                }
                Err((target, mut bound_args)) => {
                    if matches!(new_target, Value::Object(t) if Gc::ptr_eq(t, o)) {
                        new_target = target;
                    }
                    bound_args.extend(args);
                    (callee, args) = (target, bound_args);
                }
            }
        }
        let what = a.to_string(callee)?;
        Err(a.throw(ErrorKind::Type, &alloc::format!("{} is not a constructor", &**what)))
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
        let fo = ObjectData { kind: ObjectKind::Function(closure), proto: Some(proto), ..ObjectData::default() }.alloc(a.mc);
        let func = &self.function(closure.func).func;
        if func.generator {
            // 生成器函数的 `prototype` 是它产出的生成器的原型（没有 `constructor`）
            let p = ObjectData { proto: Some(a.realm.generator_proto), ..ObjectData::default() }.alloc(a.mc);
            fo.borrow_mut(a.mc).define(a.mc, "prototype", Property { slot: Slot::Data(p.into()), enumerable: false, writable: true, configurable: false });
        } else if func.kind == FunctionKind::Normal && !func.asynchronous {
            let p = a.object();
            p.borrow_mut(a.mc).define(a.mc, "constructor", Property::hidden(fo.into()));
            fo.borrow_mut(a.mc).define(a.mc, "prototype", Property { slot: Slot::Data(p.into()), enumerable: false, writable: true, configurable: false });
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
        if let Value::Object(o) = arr
            && let ObjectKind::Array(v) = &mut o.borrow_mut(a.mc).kind
        {
            v.extend(items);
        }
    }

    /// 对象字面量 / 类里定义一个属性（值或访问器函数是 `v`）。
    fn define<'gc>(a: &mut Agent<'gc, '_>, obj: Value<'gc>, key: &str, kind: PropertyKind, enumerable: bool, v: Value<'gc>) {
        let Value::Object(o) = obj else { return };
        match (kind, v) {
            (PropertyKind::Data, _) => a.define(o, key, Property { slot: Slot::Data(v), enumerable, writable: true, configurable: true }),
            (PropertyKind::Getter | PropertyKind::Setter, Value::Object(func)) => a.define_accessor(o, key, func, kind == PropertyKind::Setter, enumerable),
            _ => {}
        }
    }

    /// 跑到最外层帧返回；异常先找处理器，接不住才往外抛。
    fn execute<'gc>(&self, a: &mut Agent<'gc, '_>, mut st: State<'gc>) -> Result<Outcome<'gc>, Error> {
        let base = self.depth.get();
        let r = loop {
            match self.run(a, &mut st, base) {
                Ok(v) => break Ok(v),
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

    /// 主循环：出错直接返回，由 [`execute`](Self::execute) 展开。
    #[expect(clippy::too_many_lines, reason = "一条指令一个分支，拆开反而难读")]
    fn run<'gc>(&self, a: &mut Agent<'gc, '_>, st: &mut State<'gc>, depth: usize) -> Result<Outcome<'gc>, Error> {
        let State { regs, frames, f, acc } = st;
        loop {
            let loaded = self.function(f.func);
            let func = &loaded.func;
            let insn = func.insns[f.pc];
            f.pc += 1;
            let r = |r: Reg| f.base + usize::from(r.0);
            match insn {
                Insn::LdaUndefined => *acc = Value::Undefined,
                Insn::LdaNull => *acc = Value::Null,
                Insn::LdaBool(b) => *acc = b.into(),
                Insn::LdaConst(k) => {
                    *acc = match &func.consts[k.0 as usize] {
                        Constant::Number(x) => (*x).into(),
                        Constant::String(s) => a.string(s).into(),
                    }
                }
                Insn::Ldar(x) => *acc = regs[r(x)],
                Insn::Star(x) => regs[r(x)] = *acc,
                Insn::LdaGlobal(k) => {
                    let name = func.name_at(k);
                    let found = a.realm.global.borrow().own(name);
                    *acc = found.ok_or_else(|| a.throw(ErrorKind::Reference, &alloc::format!("{name} is not defined")))?;
                }
                Insn::LdaGlobalOrUndefined(k) => *acc = a.realm.global.borrow().own(func.name_at(k)).unwrap_or_default(),
                Insn::StaGlobal(k) => a.realm.global.borrow_mut(a.mc).set(a.mc, func.name_at(k), *acc),
                Insn::LdaEnv { depth, slot } => *acc = Self::env_at(f.env, depth).borrow().slots[usize::from(slot)],
                Insn::StaEnv { depth, slot } => Self::env_at(f.env, depth).borrow_mut(a.mc).slots[usize::from(slot)] = *acc,
                Insn::PushEnv(n) => f.env = Some(Env { slots: alloc::vec![Value::Undefined; usize::from(n)], parent: f.env }.alloc(a.mc)),
                Insn::PopEnv => f.env = Self::env_at(f.env, 0).borrow().parent,
                Insn::CloneEnv => {
                    let e = Self::env_at(f.env, 0);
                    let copy = Env { slots: e.borrow().slots.clone(), parent: e.borrow().parent };
                    f.env = Some(copy.alloc(a.mc));
                }
                Insn::Binary(op, x) => *acc = a.binary(op, regs[r(x)], *acc)?,
                Insn::Compare(op, x) => *acc = a.compare(op, regs[r(x)], *acc)?.into(),
                Insn::Unary(op) => *acc = a.unary(op, *acc)?,
                Insn::ToNumber => *acc = a.to_number(*acc)?.into(),
                Insn::Inc => *acc = (a.to_number(*acc)? + 1.0).into(),
                Insn::Dec => *acc = (a.to_number(*acc)? - 1.0).into(),
                Insn::Jump(l) => f.pc = l.0 as usize,
                Insn::JumpIfTrue(l) => {
                    if a.to_boolean(*acc) {
                        f.pc = l.0 as usize;
                    }
                }
                Insn::JumpIfFalse(l) => {
                    if !a.to_boolean(*acc) {
                        f.pc = l.0 as usize;
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
                    if *acc == Value::Undefined {
                        f.pc = l.0 as usize;
                    }
                }
                Insn::JumpIfNotUndefined(l) => {
                    if *acc != Value::Undefined {
                        f.pc = l.0 as usize;
                    }
                }
                Insn::CreateRest(from) => *acc = a.array(f.args.get(usize::from(from)..).unwrap_or(&[]).to_vec()).into(),
                Insn::ArrayPush { arr } => Self::push(a, regs[r(arr)], [*acc]),
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
                        Prepared::Frame(next) => frames.push(core::mem::replace(f, next)),
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
                    ctor.borrow_mut(a.mc).define(a.mc, "prototype", Property { slot: Slot::Data(p.into()), enumerable: false, writable: false, configurable: false });
                    p.borrow_mut(a.mc).define(a.mc, "constructor", Property::hidden(ctor.into()));
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
                Insn::DefineNamed { obj, name, kind, enumerable } => Self::define(a, regs[r(obj)], func.name_at(name), kind, enumerable, *acc),
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
                        Prepared::Frame(next) => frames.push(core::mem::replace(f, next)),
                        Prepared::Done(v) => *acc = v,
                    }
                }
                Insn::InstanceOf(x) => *acc = a.instance_of(regs[r(x)], *acc)?.into(),
                Insn::In(x) => *acc = a.has_keyed(*acc, regs[r(x)])?.into(),
                Insn::DeleteNamed { obj, name } => *acc = a.delete(regs[r(obj)], func.name_at(name))?.into(),
                Insn::DeleteKeyed { obj } => *acc = a.delete_keyed(regs[r(obj)], *acc)?.into(),
                Insn::CreateObject => *acc = a.object().into(),
                Insn::CreateArray { first, count } => *acc = a.array(regs[r(first)..r(first) + usize::from(count)].to_vec()).into(),
                Insn::GetNamed { obj, name } => *acc = a.get_named(regs[r(obj)], func.name_at(name))?,
                Insn::SetNamed { obj, name } => a.set_named(regs[r(obj)], func.name_at(name), *acc)?,
                Insn::GetKeyed { obj } => *acc = a.get_keyed(regs[r(obj)], *acc)?,
                Insn::SetKeyed { obj, key } => a.set_keyed(regs[r(obj)], regs[r(key)], *acc)?,
                Insn::Call { callee, this, args, argc } => {
                    let level = depth + frames.len() + 1;
                    if level >= STACK_LIMIT {
                        return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
                    }
                    let first = r(args);
                    let list = regs[first..first + usize::from(argc)].to_vec();
                    match self.resolve(a, regs[r(callee)], regs[r(this)], list, level)? {
                        Callee::Closure { closure, callee, this, args } => {
                            let next = self.enter(a, regs, closure, callee, this, &args);
                            frames.push(core::mem::replace(f, next));
                        }
                        Callee::Done(v) => *acc = v,
                    }
                }
                Insn::PushHandler(target) => f.handlers.push(Handler { target, env: f.env }),
                Insn::PopHandler => _ = f.handlers.pop(),
                Insn::Throw => return Err(a.throw_value(*acc)),
                Insn::Return => {
                    if f.new_target != Value::Undefined {
                        *acc = self.construct_result(a, f, *acc)?;
                    }
                    match frames.pop() {
                        Some(caller) => {
                            regs.truncate(f.base);
                            *f = caller;
                        }
                        None => return Ok(Outcome::Return(*acc)),
                    }
                }
                Insn::Yield { mode } | Insn::Await { mode } => {
                    debug_assert!(frames.is_empty() && f.base == 0, "a coroutine suspends from its own bottom frame");
                    let saved = Suspended {
                        func: f.func,
                        pc: f.pc,
                        regs: core::mem::take(regs),
                        env: f.env,
                        handlers: f.handlers.drain(..).map(|h| (h.target.0, h.env)).collect(),
                        args: core::mem::take(&mut f.args),
                        mode: Some(mode.0),
                    };
                    return Ok(Outcome::Suspend { value: *acc, awaiting: matches!(insn, Insn::Await { .. }), saved });
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

impl<'gc> Invoke<'gc> for Executor {
    fn call(&self, a: &mut Agent<'gc, '_>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let level = self.depth.get() + 1;
        if level >= STACK_LIMIT {
            return Err(a.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
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
        let handlers = saved.handlers.into_iter().map(|(t, env)| Handler { target: Label(t), env }).collect();
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
        let r = self.execute(a, st);
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

#[cfg(feature = "compile")]
impl r8frame::Engine for Executor {
    fn eval<'gc>(&mut self, scope: &mut r8frame::Scope<'gc, '_>, cx: r8frame::Context<'gc>, src: &r8frame::Source<'_>) -> Result<Value<'gc>, Error> {
        let main = self.load(Module::try_from(src)?);
        let this: &Executor = self;
        let mut agent = scope.agent(cx, this);
        let v = this.run_script(&mut agent, main)?;
        agent.run_jobs()?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::string::String;
    use alloc::vec::Vec;
    use core::num::NonZeroUsize;
    use core::time::Duration;
    use std::sync::Mutex;

    use r8builtins::Builtins;
    use r8frame::{CreateParams, Error, Isolate, Platform, Source, Task};

    use crate::Executor;

    /// 截下 `console.log` 的平台。
    #[derive(Default)]
    struct Capture(Mutex<Vec<String>>);

    impl Platform for Capture {
        fn workers(&self) -> NonZeroUsize {
            NonZeroUsize::MIN
        }

        fn now(&self) -> Duration {
            Duration::ZERO
        }

        fn print(&self, line: &str) {
            self.0.lock().expect("an unpoisoned log").push(String::from(line));
        }

        fn run(&self, tasks: Vec<Task<'_>>) {
            for t in tasks {
                t();
            }
        }
    }

    /// 跑一段脚本：结果（或抛出的值）按 ToString 给出，外加 `console.log` 的输出。
    fn eval(src: &str) -> (Result<String, String>, Vec<String>) {
        let platform = Capture::default();
        let mut isolate = Isolate::new(&platform, CreateParams::default());
        let mut executor = Executor::default();
        let r = isolate.enter(|scope| {
            let cx = scope.context(&Builtins);
            let r = scope.run(cx, &mut executor, &Source { text: src, file: "t.js" });
            let mut agent = scope.agent(cx, &executor);
            let (ok, v) = match r {
                Ok(v) => (true, v),
                Err(Error::Exception(g)) => (false, agent.local(&g)),
                Err(e) => return Err(alloc::format!("{e}")),
            };
            let text = agent.to_string(v).map_or_else(|_| String::from("<ToString threw>"), |s| String::from(&**s));
            if ok { Ok(text) } else { Err(text) }
        });
        (r, platform.0.into_inner().expect("an unpoisoned log"))
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
        ];
        for (src, want) in printed {
            let (r, out) = eval(src);
            assert!(r.is_ok(), "{src}: {r:?}");
            assert_eq!(out.join("|"), want, "{src}");
        }
    }
}
