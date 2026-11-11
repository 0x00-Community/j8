use gc_arena::{Collect, DynamicRootSet, Mutation};

use crate::{Context, Error, JsObject, Platform, Profiler, Scope, Value};

/// 动态代码的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodeKind {
    /// 间接 `eval` / `Function` 构造器：顶层 `let` / `const` 只在这段代码里
    Eval,
    /// 严格代码里的直接 `eval`：代码本身严格，`var` 与函数声明也只在它自己里面
    StrictEval,
    /// 一段新脚本（`$262.evalScript`）：顶层词法声明进脚本作用域
    Script,
}

/// 恢复协程的方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Collect)]
#[collect(require_static)]
pub enum Resume {
    Next,
    Throw,
    Return,
}

impl Resume {
    pub const ALL: [Resume; 3] = [Resume::Next, Resume::Throw, Resume::Return];
}

/// 恢复方式写进寄存器时的数（编译器在 `yield` 之后按它分派）。
impl From<Resume> for f64 {
    fn from(m: Resume) -> f64 {
        match m {
            Resume::Next => 0.0,
            Resume::Throw => 1.0,
            Resume::Return => 2.0,
        }
    }
}

/// 协程跑一步的结果。
#[derive(Clone, Copy, Debug)]
pub enum Step<'gc> {
    /// 停在 `yield`
    Yield(Value<'gc>),
    /// 停在 `await`
    Await(Value<'gc>),
    /// 跑完了（返回值）
    Done(Value<'gc>),
}

/// 执行引擎提供的回调：运算（访问器、ToPrimitive、宿主函数）要调回 JS 时经它。
pub trait Invoke<'gc> {
    /// 调一个值；不可调就抛 TypeError。
    fn call(&self, agent: &mut Agent<'gc, '_>, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error>;

    /// 构造调用（`new callee(...args)`，`new.target` 是 `new_target`）；不可构造就抛 TypeError。
    fn construct(&self, agent: &mut Agent<'gc, '_>, callee: Value<'gc>, args: &[Value<'gc>], new_target: Value<'gc>) -> Result<Value<'gc>, Error>;

    /// 编译并在当前执行环境跑一段代码，给出完成值；语法错抛 SyntaxError，没有编译器的引擎也抛 SyntaxError。
    fn eval_code(&self, agent: &mut Agent<'gc, '_>, src: &str, kind: CodeKind) -> Result<Value<'gc>, Error>;
    /// 这个闭包能不能 `new`（普通函数与类构造器能；箭头函数、方法、生成器、async 函数不能）。
    fn constructs(&self, closure: &crate::Closure<'gc>) -> bool;

    /// 恢复一个协程（`ObjectKind::Coroutine`）跑一步：`Next` 把 `value` 当 `yield` / `await` 的结果，
    /// `Throw` 在挂起处抛出，`Return` 在挂起处返回（经过 finally）。
    fn resume(&self, agent: &mut Agent<'gc, '_>, coroutine: crate::JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<Step<'gc>, Error>;
}

/// 一次运行的全部上下文：GC 堆、平台、剖析器、执行环境、回调。JS 的运算语义都挂在它上面。
pub struct Agent<'gc, 'x> {
    pub mc: &'gc Mutation<'gc>,
    pub(crate) roots: &'gc DynamicRootSet<'gc>,
    pub platform: &'x dyn Platform,
    pub profiler: &'x mut Profiler,
    pub realm: Context<'gc>,
    /// 正在跑的宿主函数是构造调用时的 `new.target`，普通调用是 `undefined`（执行器调宿主函数前设好）
    pub new_target: Value<'gc>,
    invoke: &'x dyn Invoke<'gc>,
}

impl<'gc> Scope<'gc, '_> {
    /// 在执行环境 `realm` 里、以 `invoke` 回调 JS，借出一个 agent。
    pub fn agent<'x>(&'x mut self, realm: Context<'gc>, invoke: &'x dyn Invoke<'gc>) -> Agent<'gc, 'x> {
        Agent { mc: self.mc, roots: self.roots, platform: self.platform, profiler: &mut *self.profiler, realm, new_target: Value::Undefined, invoke }
    }
}

impl<'gc> Agent<'gc, '_> {
    /// 调一个 JS 值。
    pub fn call(&mut self, callee: Value<'gc>, this: Value<'gc>, args: &[Value<'gc>]) -> Result<Value<'gc>, Error> {
        let invoke = self.invoke;
        invoke.call(self, callee, this, args)
    }

    /// IsConstructor。
    pub fn is_constructor(&self, v: Value<'gc>) -> bool {
        let Value::Object(o) = v else { return false };
        match &o.borrow().kind {
            crate::ObjectKind::Function(c) => self.invoke.constructs(c),
            crate::ObjectKind::Native(n) => n.constructor,
            crate::ObjectKind::Bound(b) => self.is_constructor(b.target),
            crate::ObjectKind::Proxy(p) => p.constructor,
            _ => false,
        }
    }

    /// GetPrototypeFromConstructor：`new_target.prototype` 是对象就用它，否则用 `fallback`。
    pub fn proto_from_ctor(&mut self, new_target: Value<'gc>, fallback: JsObject<'gc>) -> Result<JsObject<'gc>, Error> {
        if new_target == Value::Undefined {
            return Ok(fallback);
        }
        match self.get_named(new_target, "prototype")? {
            Value::Object(p) => Ok(p),
            _ => Ok(fallback),
        }
    }

    /// OrdinaryCreateFromConstructor：按当前宿主构造调用的 `new.target` 定原型（普通调用用 `fallback`）。
    pub fn create_from_ctor(&mut self, fallback: JsObject<'gc>, kind: crate::ObjectKind<'gc>) -> Result<JsObject<'gc>, Error> {
        let proto = self.proto_from_ctor(self.new_target, fallback)?;
        Ok(crate::ObjectData { kind, proto: Some(proto), ..crate::ObjectData::default() }.alloc(self.mc))
    }

    /// Boolean / Number / String 构造器的结尾：普通调用给原始值，构造调用包成包装对象。
    pub fn wrap_primitive(&mut self, v: Value<'gc>, fallback: JsObject<'gc>) -> Result<Value<'gc>, Error> {
        if self.new_target == Value::Undefined {
            return Ok(v);
        }
        Ok(self.create_from_ctor(fallback, crate::ObjectKind::Primitive(v))?.into())
    }

    /// 编译并跑一段动态代码。
    pub fn eval_code(&mut self, src: &str, kind: CodeKind) -> Result<Value<'gc>, Error> {
        let invoke = self.invoke;
        invoke.eval_code(self, src, kind)
    }

    /// 恢复协程一步。
    pub fn resume(&mut self, coroutine: crate::JsObject<'gc>, mode: Resume, value: Value<'gc>) -> Result<Step<'gc>, Error> {
        let invoke = self.invoke;
        invoke.resume(self, coroutine, mode, value)
    }

    /// 构造调用。
    pub fn construct(&mut self, callee: Value<'gc>, args: &[Value<'gc>], new_target: Value<'gc>) -> Result<Value<'gc>, Error> {
        let invoke = self.invoke;
        invoke.construct(self, callee, args, new_target)
    }
}
