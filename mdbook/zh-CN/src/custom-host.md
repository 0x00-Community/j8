# 用宏开发自定义内容：迭代器与异步

[绑定宏](bindings.md)逐个列了属性的作用。本章从头到尾做一个宿主扩展，重点讲 JavaScript 与 Rust 需要来回交接控制权的两处：
**迭代器**和**异步**。下面每一段代码都出自同一个编译并跑过的程序，输出在最后。

## 一张图看懂桥

```text
 JS 调用 ─▶ 生成的包装 ─▶ 你的 Rust fn(&mut Scope<'scope>, …)                 ◀── 同步：值只活在 'scope 里
                                │
                                ├─ 返回 T: IntoJs ─────────────────────────▶ JS 值
                                │
                                └─ Glue::spawn(future) ─▶ promise ─────────▶ JS 立刻拿到 promise
                                               │
                   事件循环（run_until_idle）轮询 future：Send + 'static，没有 Scope，没有 Value
                                               │
                                   future 返回一个 Settle ─▶ 带 &mut Scope 运行 ─▶ resolve / reject
```

由此得出三条规则，本章所有写法都是它们的推论：

1. **`Value<'scope>` / `JsObject<'scope>` 只在一次调用里存在。** 要把 JS 值留得更久，存
   `ScopeGlobal<'p, ValueBrand>`（根，`Send`）；在 class 里，`Value<'scope>` 字段是被追踪的边。
2. **future 里没有 `Scope`。** 它不能读 JS 值、不能调 JS 函数、不能推进 JS 迭代器，只能计算和等待；只有在最后，通过它
   返回的 `Settle`，才拿回一次 `Scope`。
3. **future 必须唤醒它的 waker**，否则 `run_until_idle` 会一直睡下去（见[异步与 Promise](async.md)）。

## 准备

```toml
[dependencies]
j8 = { version = "0.4", features = ["runtime", "json"] }
```

```rust,ignore
use j8::executor::Executor;
use j8::frame::{Scope, Error, ErrorKind, Settle, Value};
use j8::platform::DefaultPlatform;
use j8::{Function, Glue, IntoJs, Promise};
```

## 把迭代结果做成 Rust 类型

两种迭代器最后都落到同一个对象 `{ value, done }`。把它写成一个 Rust 类型，`IntoJs` 里调 `Scope::iter_result`。
`Step<T>` 是普通数据（`T` 是 `Send` 它就是 `Send`），所以既能从同步方法返回，**也**能从还没有 `Scope` 的 future 里带出来：

```rust,ignore
pub struct Step<T>(pub Option<T>);   // Some(v) ⇒ { value: v, done: false }，None ⇒ { value: undefined, done: true }

impl<'scope, T: IntoJs<'scope>> IntoJs<'scope> for Step<T> {
    fn into_js(self, a: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        let (value, done) = match self.0 {
            Some(v) => (v.into_js(a)?, false),
            None => (Value::Undefined, true),
        };
        Ok(a.iter_result(value, done).into())
    }
}
```

## 迭代器：Rust ⇒ JS

JS 迭代器就是带 `next()` 的对象。一个有 `next` 方法的 `#[j8::class]` 就是迭代器；再加 `return` 方法，提前退出
（`break`、解构、循环体抛错）就能通知到 Rust。

```rust,ignore
#[derive(j8::Trace)]
struct Range { next: f64, end: f64, step: f64, #[collect(require_static)] closed: Arc<AtomicBool> }

#[j8::class]
impl Range {
    #[js(constructor)]
    fn new(a: &mut Scope<'_, '_>, end: f64, step: Option<f64>) -> Result<Self, Error> {
        let step = step.unwrap_or(1.0);
        if step <= 0.0 {
            return Err(a.throw(ErrorKind::Range, "step must be positive"));
        }
        Ok(Self { next: 0.0, end, step, closed: Arc::default() })
    }

    fn next(&mut self) -> Step<f64> {
        if self.closed.load(Ordering::Acquire) || self.next >= self.end {
            return Step(None);
        }
        let v = self.next;
        self.next += self.step;
        Step(Some(v))
    }

    #[js(name = "return")]          // return 是 Rust 关键字：在 JS 里起名
    fn finish(&mut self) -> Step<f64> {
        self.closed.store(true, Ordering::Release);   // 在这里释放文件、套接字……
        Step(None)
    }

    #[js(getter)]
    fn closed(&self) -> bool { self.closed.load(Ordering::Acquire) }
}
```

类还缺 `[Symbol.iterator]`。最短又正确的做法是 prelude 里一行，把 `Iterator.prototype` 挂到类原型下面：它提供
`[Symbol.iterator]() { return this }`，**也**带来迭代器辅助方法（`map`、`filter`、`take`、`toArray`……）。

```js
Object.setPrototypeOf(Range.prototype, Iterator.prototype);
```

```js
[...new Range(5)]                                                  // 0,1,2,3,4
new Range(10, 2).map(x => x * 10).filter(x => x > 20).toArray()    // 40,60,80
const r = new Range(100);
for (const x of r) { if (x === 3) break; }
r.closed                                                           // true：break 调了 return()
(([a, b]) => a + b)(new Range(9, 4))                               // 4：解构也会关闭
```

只要普通 `for…of`、不要辅助方法时，prelude 那行也可以写成
`Range.prototype[Symbol.iterator] = function () { return this; };`。

结果小而有限时，上面都不用：直接返回 `Vec<T>`，到 JS 里就是数组。

## 迭代器：JS ⇒ Rust

**一次读完：** `Vec<T>` 参数接受**任何**可迭代对象（数组、`Set`、生成器、上面的 `Range`），在你的函数运行前按迭代协议读完。

```rust,ignore
#[j8::object("sink")]
impl Sink {
    fn total(xs: Vec<f64>) -> f64 { xs.iter().sum() }      // sink.total(new Set([1, 2, 3])) === 6
}
```

**按需读：** 把可迭代对象当 `Value` 收下，再加 `&mut Scope`，自己一步步推。`get_iterator` 调 `[Symbol.iterator]`，
`iterator_step` 调 `next()`、`done` 时返回 `None`，`iterator_close` 调 `return()`。**没读完就停时一定要关闭迭代器**，
生产方的清理才会运行（生成器的 `finally`、`Range` 的 `return`）：

```rust,ignore
fn sum_until<'scope>(a: &mut Scope<'scope, '_>, iterable: Value<'scope>, limit: f64) -> Result<f64, Error> {
    let it = a.get_iterator(iterable)?;
    let mut sum = 0.0;
    while let Some(v) = a.iterator_step(it)? {
        sum += a.to_number(v)?;
        if sum >= limit {
            a.iterator_close(it.iterator)?;        // 提前停：关闭
            break;
        }
    }
    Ok(sum)
}

fn each<'scope>(a: &mut Scope<'scope, '_>, iterable: Value<'scope>, f: Function) -> Result<u32, Error> {
    let it = a.get_iterator(iterable)?;
    let mut n = 0;
    while let Some(v) = a.iterator_step(it)? {
        if let Err(e) = f.call(a, Value::Undefined, &[v]) {
            a.iterator_close(it.iterator)?;        // 回调抛错：先关闭，再把错抛出去
            return Err(e);
        }
        n += 1;
    }
    Ok(n)
}
```

```js
let cleaned = false;
function* naturals() { try { for (let i = 1; ; i++) yield i; } finally { cleaned = true; } }
sink.sumUntil(naturals(), 10)        // 10，且 cleaned === true
sink.each(naturals(), x => { if (x === 2) throw new Error('stop at ' + x); })   // 抛出 "stop at 2"
```

注意名字：`sum_until` 在 JS 里是 `sumUntil`。导出的名字一律转成 camelCase，除非写了 `#[js(name = "…")]`。

`next()` 本身（也就是 `iterator_step`）抛出的错已经是 JS 异常：用 `?` 往外传、不要关闭——规范规定抛过错的迭代器已经结束了。

## 异步：Rust ⇒ JS

**`async fn`** 最省事。它只收自有参数（不能有 `self`、`&str`、`Scope`）；调用立刻返回 promise，兑现时结果经 `IntoJs`
转换，`Result::Err` 则 reject。

```rust,ignore
async fn doubled(p: Promise<f64>) -> Result<f64, Error> {
    Ok(p.await? * 2.0)
}
```

**`Glue::spawn`** 用来做 `async fn` 做不到的事：用 `&self` / `&mut self`、借 `Scope` 读 JS 参数、或者同步地决定要不要开工。
先把 JS 那边的活做完，只把 `Send` 的数据移进 future，future 最后返回 `Glue::settle(value)`：

```rust,ignore
fn later<'scope>(&self, a: &mut Scope<'scope, '_>, ms: f64) -> Result<Value<'scope>, Error> {
    let wait = Duration::from_secs_f64(ms / 1000.0);
    let label = self.label.clone();                         // 从 self 里克隆出来：future 是 'static
    Glue::spawn(a, async move {
        Offload::new(move || std::thread::sleep(wait)).await;   // 阻塞的活放到会唤醒 waker 的线程上
        Glue::settle(label)
    })
}
```

`Offload` 就是[异步与 Promise](async.md) 里「线程 + waker」的那个 future。

**活干完时回到 JS。** `Glue::settle` 只是转换一个值。future 返回的 `Settle` 其实是
`Box<dyn FnOnce(&mut Scope) -> Result<Value, Error> + Send>`，所以可以自己写，在那一刻做任何需要 `Scope` 的事——
这里调一个 JS 回调，它的返回值成为 promise 的值：

```rust,ignore
fn after<'scope>(a: &mut Scope<'scope, '_>, ms: f64, f: Function) -> Result<Value<'scope>, Error> {
    let wait = Duration::from_secs_f64(ms / 1000.0);
    Glue::spawn(a, async move {
        Offload::new(move || std::thread::sleep(wait)).await;
        let settle: Settle = Box::new(move |a| f.call(a, Value::Undefined, &[]));
        settle                                               // 这个类型标注不能省
    })
}
```

```js
await timers.after(5, () => 'callback ran')          // 'callback ran'
```

## 异步：JS ⇒ Rust

**`Promise<T>` 参数**把 JS promise（或者任何值：按 `await` 的方式接收，thenable 也行）变成 Rust 里
`Result<T, Error>` 的 future。reject 到 Rust 是 `Error::Exception`；用 `?` 原样返回，你自己的 promise 就以**同一个**
JS 值 reject，`instanceof` 和 `message` 都保留：

```js
await sink.doubled(Promise.resolve(21))                       // 42
try { await sink.doubled(Promise.reject(new TypeError('nope'))) }
catch (e) { e instanceof TypeError && e.message === 'nope' }  // true
```

**调一个 JS 异步函数并在 Rust 里 await 它：** 趁手里还有 `Scope` 时调用，用 `FromJs` 把它的 promise 转过来，再在 future 里
await：

```rust,ignore
fn call_async<'scope>(a: &mut Scope<'scope, '_>, f: Function, x: f64) -> Result<Value<'scope>, Error> {
    let p = f.call(a, Value::Undefined, &[x.into()])?;
    let p = <Promise<String> as j8::FromJs>::from_js(a, p)?;
    Glue::spawn(a, async move { Glue::settle(p.await.map(|s| format!("rust saw {s}"))) })
}
```

```js
await sink.callAsync(async x => 'js ' + (x + 1), 41)        // 'rust saw js 42'
```

`T` 要求 `FromJs` 且 `Send + 'static`（`f64`、`String`、`Vec<…>`、`j8::Json<T>`、`j8::Bytes`……）：promise 兑现时在
JS 这边转换，只有 Rust 结果被送进 future。

## 异步迭代器：Rust ⇒ JS

异步迭代器就是 `next()` 返回 `{ value, done }` 的 promise 的对象。把上面两半拼起来：`next` 是 `&mut self` 方法，同步地
更新状态，返回 `Glue::spawn(…)`，future 以 `Glue::settle(Step(…))` 结束。

```rust,ignore
#[derive(j8::Trace)]
struct Ticker { left: u32, every: Duration }

#[j8::class]
impl Ticker {
    #[js(constructor)]
    fn new(count: u32, ms: f64) -> Self {
        Self { left: count, every: Duration::from_secs_f64(ms / 1000.0) }
    }

    fn next<'scope>(&mut self, a: &mut Scope<'scope, '_>) -> Result<Value<'scope>, Error> {
        let tick = (self.left > 0).then_some(self.left);    // 状态现在就改，按调用顺序
        self.left = self.left.saturating_sub(1);
        let every = self.every;
        Glue::spawn(a, async move {
            let tick = match tick {
                Some(n) => Some(Offload::new(move || { std::thread::sleep(every); n }).await),
                None => None,
            };
            Glue::settle(Step(tick))
        })
    }
}
```

```js
Ticker.prototype[Symbol.asyncIterator] = function () { return this; };   // prelude
```

```js
for await (const t of new Ticker(3, 5)) ticks.push(t);      // 3,2,1
```

状态要在 `next` 里改，不要放进 future：`for await` 会等每个 promise，但不 await 就连调几次 `next()` 的代码也必须按顺序拿到元素。

## 异步迭代器：JS ⇒ Rust

这是 Rust future 唯一独自推不动的方向：推进 JS 异步迭代器要在每次 await 之后再调 `next()`，而 future 里没有 `Scope`
（规则 2）。让 JavaScript 来跑循环、把每个元素交给 Rust——在绑定类上加几行 prelude：

```rust,ignore
#[derive(j8::Trace)]
struct Collector { items: Vec<String> }

#[j8::class]
impl Collector {
    #[js(constructor)]
    fn new() -> Self { Self { items: Vec::new() } }
    fn push(&mut self, item: String) { self.items.push(item); }
    #[js(getter)]
    fn items(&self) -> Vec<String> { self.items.clone() }
}
```

```js
Collector.prototype.drain = async function (source) {      // prelude
    for await (const x of source) this.push(x);
    return this;
};
```

```js
async function* words() { yield 'a'; await null; yield 'b'; }
(await new Collector().drain(words())).items               // ['a', 'b']
```

提前退出、`return()` 和错误都由 `for await` 自己处理。如果 Rust 要把元素当流来消费，让 `push` 把它们送进一个通道，由 Rust
future 去读。

## 拼起来

```rust,ignore
j8::runtime! {
    pub Streams {
        globals: [Range, Ticker, Sink, Collector, Timers],
        prelude: "
            Object.setPrototypeOf(Range.prototype, Iterator.prototype);
            Ticker.prototype[Symbol.asyncIterator] = function () { return this; };
            Collector.prototype.drain = async function (source) {
                for await (const x of source) this.push(x);
                return this;
            };
        ",
    }
}

fn main() -> Result<(), Error> {
    let platform = Streams::platform(DefaultPlatform::default());
    j8::EventLoopSet::scope(&platform, NonZeroUsize::MIN, |set| {
        let rt = Streams::runtime(set, Executor::default())?;
        rt.eval(SCRIPT, "main.js")?;      // 上面那些 JS 片段，结果记进 globalThis.out
        rt.run_until_idle()?;             // ⇐ 推动所有 promise 和 future；不调它，异步的东西都不会兑现
        let out = rt.eval("out.join('\\n')", "print.js")?;
        println!("{}", rt.text(&out));
        Ok(())
    })
}
```

```text
for-of 0,1,2,3,4
helpers 40,60,80
break closes true
destructure 4
total 6 6
sumUntil 10 true
each threw stop at 2
for-await 3,2,1
doubled 42
callAsync rust saw js 42
drain a,b
after callback ran
rejected true nope
```

## 速查

| 想要 | 用 |
|---|---|
| JS 遍历 Rust 数据，量小 | 返回 `Vec<T>` |
| JS 遍历 Rust 数据，按需 | 带 `next`（加 `#[js(name = "return")]`）、返回 `Step<T>` 的类；prelude 把 `Iterator.prototype` 挂到它下面 |
| Rust 读 JS 可迭代对象，全部 | `Vec<T>` 参数 |
| Rust 读 JS 可迭代对象，按需 | `Value` + `get_iterator` / `iterator_step`；每次提前停都 `iterator_close` |
| JS await Rust 的活 | `async fn`（自有参数）或 `Glue::spawn` + `Glue::settle` |
| Rust await JS promise | `Promise<T>` 参数，或 `f.call(…)` 后 `Promise::<T>::from_js` |
| Rust 活干完时调 JS | 返回自己写的 `Settle` 闭包 |
| JS 对 Rust 做 `for await` | `next` 返回 `Glue::spawn(… Step …)` 的类；prelude 设 `Symbol.asyncIterator` |
| Rust 消费 JS 异步迭代器 | prelude 里 `for await`，把元素 push 进绑定类 |

坑：名字会转 camelCase（`sum_until` ⇒ `sumUntil`）；`&self` 方法调 JS、JS 又可变地重入同一对象会抛 `TypeError`；
⛔ 把 `Value` 存进结构体或移进 future；不唤醒 waker 的 future 会让 `run_until_idle` 卡死；不是 `j8::Runtime` 建的、没有事件循环的
isolate 里，`Glue::spawn` 抛 `TypeError`。
