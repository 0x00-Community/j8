//! r8executor：r8bytecode 字节码执行器（`no_std`）。
//!
//! 显式帧栈（不靠宿主递归），所有帧共用一个寄存器文件；被捕获的绑定在堆上的 [`Env`] 链里。
//! 异常：每帧一个动态处理器栈（`PushHandler` / `PopHandler`），抛出时弹本帧处理器，没有就展开到调用者。
//! 一个 `Executor` 只配一个 isolate：它装着已加载的函数，堆里的闭包按 [`FuncId`] 指回来。
#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use core::cell::Cell;

use r8bytecode::{Constant, Function, Insn, Label, Module, Reg};
use r8frame::{Agent, Closure, Env, Error, ErrorKind, FuncId, Invoke, JsEnv, ObjectData, ObjectKind, Value};

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

/// 调用解析的结果：要进一帧的闭包，或宿主函数已经算完的值。
enum Callee<'gc> {
    Closure { closure: Closure<'gc>, callee: Value<'gc>, this: Value<'gc>, args: Vec<Value<'gc>> },
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
        let f = self.enter(a, &mut regs, Closure { func: main, env: None }, Value::Undefined, this, &[]);
        self.execute(a, State { regs, frames: Vec::new(), f, acc: Value::Undefined })
    }

    /// 解开绑定函数、直接调宿主函数；剩下 JS 闭包交给调用者进帧。
    fn resolve<'gc>(&self, a: &mut Agent<'gc, '_>, mut callee: Value<'gc>, mut this: Value<'gc>, mut args: Vec<Value<'gc>>, depth: usize) -> Result<Callee<'gc>, Error> {
        while let Value::Object(o) = callee {
            let kind = match &o.borrow().kind {
                ObjectKind::Function(c) => return Ok(Callee::Closure { closure: *c, callee, this, args }),
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
        Frame { func: closure.func, pc: 0, base, env: closure.env, handlers: Vec::new() }
    }

    /// 往外第 `depth` 层环境。
    fn env_at(env: Option<JsEnv<'_>>, depth: u16) -> JsEnv<'_> {
        let mut e = env.expect("a scope with an environment");
        for _ in 0..depth {
            e = e.borrow().parent.expect("an enclosing environment");
        }
        e
    }

    /// 跑到最外层帧返回；异常先找处理器，接不住才往外抛。
    fn execute<'gc>(&self, a: &mut Agent<'gc, '_>, mut st: State<'gc>) -> Result<Value<'gc>, Error> {
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
    fn run<'gc>(&self, a: &mut Agent<'gc, '_>, st: &mut State<'gc>, depth: usize) -> Result<Value<'gc>, Error> {
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
                Insn::CreateClosure(n) => {
                    let closure = Closure { func: FuncId(loaded.base + n), env: f.env };
                    *acc = ObjectData { kind: ObjectKind::Function(closure), proto: Some(a.realm.function_proto), ..ObjectData::default() }.alloc(a.mc).into();
                }
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
                Insn::Return => match frames.pop() {
                    Some(caller) => {
                        regs.truncate(f.base);
                        *f = caller;
                    }
                    None => return Ok(*acc),
                },
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
                let saved = self.depth.replace(level);
                let r = self.execute(a, State { regs, frames: Vec::new(), f, acc: Value::Undefined });
                self.depth.set(saved);
                r
            }
        }
    }
}

#[cfg(feature = "compile")]
impl r8frame::Engine for Executor {
    fn eval<'gc>(&mut self, scope: &mut r8frame::Scope<'gc, '_>, cx: r8frame::Context<'gc>, src: &r8frame::Source<'_>) -> Result<Value<'gc>, Error> {
        let main = self.load(Module::try_from(src)?);
        let this: &Executor = self;
        let mut agent = scope.agent(cx, this);
        this.run_script(&mut agent, main)
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
        ];
        for (src, want) in thrown {
            assert_eq!(eval(src).0, Err(String::from(want)), "{src}");
        }
        let (r, out) = eval("console.log('a', 1, [2, 3]); console.log(); 0");
        assert_eq!((r, out), (Ok(String::from("0")), alloc::vec![String::from("a 1 2,3"), String::new()]));
    }
}
