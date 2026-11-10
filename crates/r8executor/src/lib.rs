//! r8executor：r8bytecode 字节码执行器（`no_std`）。
//!
//! 显式帧栈（不靠宿主递归），所有帧共用一个寄存器文件；被捕获的绑定在堆上的 [`Env`] 链里。
//! 一个 `Executor` 只配一个 isolate：它装着已加载的函数，堆里的闭包按 [`FuncId`] 指回来。
#![no_std]
extern crate alloc;

use alloc::vec::Vec;

use r8bytecode::{Constant, Function, Insn, Module, Reg};
use r8frame::{Closure, Context, Env, Error, ErrorKind, FuncId, JsEnv, ObjectData, ObjectKind, Scope, Value};

/// 调用深度上限，过了抛 RangeError
const STACK_LIMIT: usize = 10_000;

/// 一个已加载的函数：本体加它所在模块的第一个函数编号（`CreateClosure` 的下标相对它）。
struct Loaded {
    base: u32,
    func: Function,
}

/// 一个调用帧。
struct Frame<'gc> {
    func: FuncId,
    pc: usize,
    /// 寄存器文件里本帧的起点
    base: usize,
    env: Option<JsEnv<'gc>>,
}

/// 字节码执行器。
#[derive(Default)]
pub struct Executor {
    code: Vec<Loaded>,
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

    /// 在 `cx` 里跑脚本 `main`（`this` 是全局对象）。
    pub fn run_script<'gc>(&self, scope: &mut Scope<'gc, '_>, cx: Context<'gc>, main: FuncId) -> Result<Value<'gc>, Error> {
        let closure = Closure { func: main, env: None };
        self.invoke(scope, cx, closure, Value::Object(cx.global), &[])
    }

    /// 调一个 JS 值：不是函数就抛 TypeError。
    pub fn call<'gc>(&self, scope: &mut Scope<'gc, '_>, cx: Context<'gc>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let closure = Self::closure(scope, callee)?;
        let mut regs = Vec::new();
        let frame = self.enter(scope, &mut regs, closure, callee, this, args);
        self.execute(scope, cx, regs, frame)
    }

    fn invoke<'gc>(&self, scope: &mut Scope<'gc, '_>, cx: Context<'gc>, closure: Closure<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let mut regs = Vec::new();
        let frame = self.enter(scope, &mut regs, closure, Value::Undefined, this, args);
        self.execute(scope, cx, regs, frame)
    }

    fn closure<'gc>(scope: &Scope<'gc, '_>, v: Value<'gc>) -> Result<Closure<'gc>, Error> {
        if let Value::Object(o) = v
            && let ObjectKind::Function(c) = o.borrow().kind
        {
            return Ok(c);
        }
        let what = scope.to_string(v);
        Err(scope.throw(ErrorKind::Type, &alloc::format!("{} is not a function", &**what)))
    }

    /// 建一帧：在寄存器文件末尾开出本函数的寄存器，按调用约定放 `this`、自身、实参。
    fn enter<'gc>(&self, scope: &mut Scope<'gc, '_>, regs: &mut Vec<Value<'gc>>, closure: Closure<'gc>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Frame<'gc> {
        scope.profiler.enter(closure.func);
        let f = &self.function(closure.func).func;
        let base = regs.len();
        regs.resize(base + usize::from(f.regs), Value::Undefined);
        regs[base + usize::from(Reg::THIS.0)] = this;
        regs[base + usize::from(Reg::CALLEE.0)] = callee;
        let params = usize::from(f.params).min(args.len());
        let first = base + usize::from(Reg::PARAMS);
        regs[first..first + params].copy_from_slice(&args[..params]);
        Frame { func: closure.func, pc: 0, base, env: closure.env }
    }

    /// 往外第 `depth` 层环境。
    fn env_at(env: Option<JsEnv<'_>>, depth: u16) -> JsEnv<'_> {
        let mut e = env.expect("a scope with an environment");
        for _ in 0..depth {
            e = e.borrow().parent.expect("an enclosing environment");
        }
        e
    }

    /// 主循环：一直跑到最外层帧返回。
    fn execute<'gc>(&self, scope: &mut Scope<'gc, '_>, cx: Context<'gc>, mut regs: Vec<Value<'gc>>, entry: Frame<'gc>) -> Result<Value<'gc>, Error> {
        let mut frames: Vec<Frame<'gc>> = Vec::new();
        let mut f = entry;
        let mut acc = Value::Undefined;
        loop {
            let loaded = self.function(f.func);
            let func = &loaded.func;
            let insn = func.insns[f.pc];
            f.pc += 1;
            let r = |r: Reg| f.base + usize::from(r.0);
            match insn {
                Insn::LdaUndefined => acc = Value::Undefined,
                Insn::LdaNull => acc = Value::Null,
                Insn::LdaBool(b) => acc = b.into(),
                Insn::LdaConst(k) => {
                    acc = match &func.consts[k.0 as usize] {
                        Constant::Number(x) => (*x).into(),
                        Constant::String(s) => scope.string(s).into(),
                    }
                }
                Insn::Ldar(x) => acc = regs[r(x)],
                Insn::Star(x) => regs[r(x)] = acc,
                Insn::LdaGlobal(k) => {
                    let name = func.name_at(k);
                    acc = cx.global.borrow().own(name).ok_or_else(|| scope.throw(ErrorKind::Reference, &alloc::format!("{name} is not defined")))?;
                }
                Insn::LdaGlobalOrUndefined(k) => acc = cx.global.borrow().own(func.name_at(k)).unwrap_or_default(),
                Insn::StaGlobal(k) => cx.global.borrow_mut(scope.mc).set(scope.mc, func.name_at(k), acc),
                Insn::LdaEnv { depth, slot } => acc = Self::env_at(f.env, depth).borrow().slots[usize::from(slot)],
                Insn::StaEnv { depth, slot } => Self::env_at(f.env, depth).borrow_mut(scope.mc).slots[usize::from(slot)] = acc,
                Insn::PushEnv(n) => f.env = Some(Env { slots: alloc::vec![Value::Undefined; usize::from(n)], parent: f.env }.alloc(scope.mc)),
                Insn::PopEnv => f.env = Self::env_at(f.env, 0).borrow().parent,
                Insn::CloneEnv => {
                    let e = Self::env_at(f.env, 0);
                    let copy = { Env { slots: e.borrow().slots.clone(), parent: e.borrow().parent } };
                    f.env = Some(copy.alloc(scope.mc));
                }
                Insn::Binary(op, x) => acc = scope.binary(op, regs[r(x)], acc),
                Insn::Compare(op, x) => acc = scope.compare(op, regs[r(x)], acc).into(),
                Insn::Unary(op) => acc = scope.unary(op, acc),
                Insn::ToNumber => acc = scope.to_number(acc).into(),
                Insn::Inc => acc = (scope.to_number(acc) + 1.0).into(),
                Insn::Dec => acc = (scope.to_number(acc) - 1.0).into(),
                Insn::Jump(l) => f.pc = l.0 as usize,
                Insn::JumpIfTrue(l) => {
                    if scope.to_boolean(acc) {
                        f.pc = l.0 as usize;
                    }
                }
                Insn::JumpIfFalse(l) => {
                    if !scope.to_boolean(acc) {
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
                    acc = ObjectData::from(ObjectKind::Function(closure)).alloc(scope.mc).into();
                }
                Insn::CreateObject => acc = ObjectData::default().alloc(scope.mc).into(),
                Insn::CreateArray { first, count } => {
                    let items = regs[r(first)..r(first) + usize::from(count)].to_vec();
                    acc = ObjectData::from(ObjectKind::Array(items)).alloc(scope.mc).into();
                }
                Insn::GetNamed { obj, name } => acc = scope.get_named(regs[r(obj)], func.name_at(name))?,
                Insn::SetNamed { obj, name } => scope.set_named(regs[r(obj)], func.name_at(name), acc)?,
                Insn::GetKeyed { obj } => acc = scope.get_keyed(regs[r(obj)], acc)?,
                Insn::SetKeyed { obj, key } => scope.set_keyed(regs[r(obj)], regs[r(key)], acc)?,
                Insn::Call { callee, this, args, argc } => {
                    if frames.len() >= STACK_LIMIT {
                        return Err(scope.throw(ErrorKind::Range, "Maximum call stack size exceeded"));
                    }
                    let callee = regs[r(callee)];
                    let closure = Self::closure(scope, callee)?;
                    let this = regs[r(this)];
                    let a = r(args);
                    let args: Vec<Value<'gc>> = regs[a..a + usize::from(argc)].to_vec();
                    let next = self.enter(scope, &mut regs, closure, callee, this, &args);
                    frames.push(core::mem::replace(&mut f, next));
                }
                Insn::Throw => return Err(Error::Exception(scope.global(acc))),
                Insn::Return => match frames.pop() {
                    Some(caller) => {
                        regs.truncate(f.base);
                        f = caller;
                    }
                    None => return Ok(acc),
                },
            }
        }
    }
}

#[cfg(feature = "compile")]
impl r8frame::Engine for Executor {
    fn eval<'gc>(&mut self, scope: &mut Scope<'gc, '_>, cx: Context<'gc>, src: &r8frame::Source<'_>) -> Result<Value<'gc>, Error> {
        let main = self.load(Module::try_from(src)?);
        self.run_script(scope, cx, main)
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use r8frame::{CreateParams, Error, Isolate, Source};
    use r8platform::DefaultPlatform;

    use crate::Executor;

    /// 跑一段脚本，结果（或抛出的值）按 ToString 给出。
    fn eval(src: &str) -> Result<String, String> {
        let platform = DefaultPlatform::default();
        let mut isolate = Isolate::new(&platform, CreateParams::default());
        let mut executor = Executor::default();
        isolate.enter(|scope| {
            let cx = scope.context();
            match scope.run(cx, &mut executor, &Source { text: src, file: "t.js" }) {
                Ok(v) => Ok(String::from(&**scope.to_string(v))),
                Err(Error::Exception(g)) => Err(String::from(&**scope.to_string(scope.local(&g)))),
                Err(e) => Err(alloc::format!("{e}")),
            }
        })
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
        ];
        for (src, want) in ok {
            assert_eq!(eval(src), Ok(String::from(want)), "{src}");
        }
        let thrown = [
            ("undefinedVar", "ReferenceError: undefinedVar is not defined"),
            ("function r() { return r(); } r()", "RangeError: Maximum call stack size exceeded"),
            ("const c = 1; c = 2", "TypeError: Assignment to constant variable."),
            ("throw 'boom'", "boom"),
            ("let u; u.x", "TypeError: cannot read property 'x' of undefined"),
        ];
        for (src, want) in thrown {
            assert_eq!(eval(src), Err(String::from(want)), "{src}");
        }
    }
}
