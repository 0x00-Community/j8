# Custom content with the macros: iterators and async

[Binding macros](bindings.md) lists what each attribute does. This chapter builds one host extension end to end and
concentrates on the two places where JavaScript and Rust have to hand control back and forth: **iterators** and
**async**. Every snippet below comes from one program that was compiled and run; its output is at the end.

## The bridge in one picture

```text
 JS call ─▶ generated wrapper ─▶ your Rust fn(&mut Agent<'gc>, …)          ◀── synchronous: values live for 'gc
                                      │
                                      ├─ returns T: IntoJs ─────────────────▶ JS value
                                      │
                                      └─ Glue::spawn(future) ─▶ promise ────▶ JS gets the promise at once
                                                     │
                       event loop (run_until_idle) polls the future: Send + 'static, no Agent, no Value
                                                     │
                                         future returns a Settle ─▶ runs with &mut Agent ─▶ resolves / rejects
```

Three rules follow from it, and everything in this chapter is a consequence of them:

1. **A `Value<'gc>` / `JsObject<'gc>` exists only during one call.** To keep a JS value longer, store a
   `j8::Persistent` or `j8::Function` (both are GC roots and `Send`).
2. **A future has no `Agent`.** It cannot read a JS value, call a JS function or step a JS iterator. It can only
   compute and wait; it gets the `Agent` back once, at the very end, through the `Settle` it returns.
3. **A future must wake its waker**, or `run_until_idle` parks forever (see [Async and promises](async.md)).

## Setup

```toml
[dependencies]
j8 = { version = "0.4", features = ["runtime", "json"] }
```

```rust,ignore
use j8::executor::Executor;
use j8::frame::{Agent, Error, ErrorKind, Settle, Value};
use j8::platform::DefaultPlatform;
use j8::{Function, Glue, IntoJs, Promise};
```

## The iterator result as a Rust type

Both kinds of iterator end in the same object, `{ value, done }`. Build it once as a Rust type whose `IntoJs` calls
`Agent::iter_result`. Because `Step<T>` is plain data (`Send` when `T` is), it can be returned from a sync method **and**
carried out of a future, where no `Agent` exists yet:

```rust,ignore
pub struct Step<T>(pub Option<T>);   // Some(v) ⇒ { value: v, done: false }, None ⇒ { value: undefined, done: true }

impl<'gc, T: IntoJs<'gc>> IntoJs<'gc> for Step<T> {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        let (value, done) = match self.0 {
            Some(v) => (v.into_js(a)?, false),
            None => (Value::Undefined, true),
        };
        Ok(a.iter_result(value, done).into())
    }
}
```

## Iterators: Rust ⇒ JS

A JS iterator is any object with `next()`. A `#[j8::class]` with a `next` method is one; a `return` method makes early
exits (`break`, destructuring, a throwing loop body) reach Rust.

```rust,ignore
struct Range { next: f64, end: f64, step: f64, closed: Arc<AtomicBool> }

#[j8::class]
impl Range {
    #[js(constructor)]
    fn new(a: &mut Agent<'_, '_>, end: f64, step: Option<f64>) -> Result<Self, Error> {
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

    #[js(name = "return")]          // `return` is a Rust keyword: name it in JS
    fn finish(&mut self) -> Step<f64> {
        self.closed.store(true, Ordering::Release);   // release files, sockets, … here
        Step(None)
    }

    #[js(getter)]
    fn closed(&self) -> bool { self.closed.load(Ordering::Acquire) }
}
```

The class still needs `[Symbol.iterator]`. The shortest correct way is one line of prelude that puts
`Iterator.prototype` under the class's prototype: it supplies `[Symbol.iterator]() { return this }` **and** the
iterator helpers (`map`, `filter`, `take`, `toArray`, …).

```js
Object.setPrototypeOf(Range.prototype, Iterator.prototype);
```

```js
[...new Range(5)]                                                  // 0,1,2,3,4
new Range(10, 2).map(x => x * 10).filter(x => x > 20).toArray()    // 40,60,80
const r = new Range(100);
for (const x of r) { if (x === 3) break; }
r.closed                                                           // true: break called return()
(([a, b]) => a + b)(new Range(9, 4))                               // 4: destructuring also closes
```

If you only want plain `for…of` without the helpers, the prelude line can be
`Range.prototype[Symbol.iterator] = function () { return this; };` instead.

For a small, finite result, skip all of this and return a `Vec<T>`: it becomes a JS array.

## Iterators: JS ⇒ Rust

**Eager:** a `Vec<T>` parameter accepts *any* iterable (arrays, `Set`, generators, the `Range` above) and drains it
with the iteration protocol before your function runs.

```rust,ignore
#[j8::object("sink")]
impl Sink {
    fn total(xs: Vec<f64>) -> f64 { xs.iter().sum() }      // sink.total(new Set([1, 2, 3])) === 6
}
```

**Lazy:** take the iterable as a `Value` together with `&mut Agent` and step it yourself. `get_iterator` calls
`[Symbol.iterator]`, `iterator_step` calls `next()` and returns `None` when `done`, and `iterator_close` calls
`return()`. **Close the iterator whenever you stop before it is done**, so the producer's cleanup runs (a generator's
`finally`, a `Range`'s `return`):

```rust,ignore
fn sum_until<'gc>(a: &mut Agent<'gc, '_>, iterable: Value<'gc>, limit: f64) -> Result<f64, Error> {
    let it = a.get_iterator(iterable)?;
    let mut sum = 0.0;
    while let Some(v) = a.iterator_step(it)? {
        sum += a.to_number(v)?;
        if sum >= limit {
            a.iterator_close(it.iterator)?;        // stopping early: close
            break;
        }
    }
    Ok(sum)
}

fn each<'gc>(a: &mut Agent<'gc, '_>, iterable: Value<'gc>, f: Function) -> Result<u32, Error> {
    let it = a.get_iterator(iterable)?;
    let mut n = 0;
    while let Some(v) = a.iterator_step(it)? {
        if let Err(e) = f.call(a, Value::Undefined, &[v]) {
            a.iterator_close(it.iterator)?;        // the callback threw: close, then rethrow
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
sink.sumUntil(naturals(), 10)        // 10, and cleaned === true
sink.each(naturals(), x => { if (x === 2) throw new Error('stop at ' + x); })   // throws "stop at 2"
```

Note the names: `sum_until` is `sumUntil` in JS. Every exported name is converted to camelCase unless you give
`#[js(name = "…")]`.

Errors from `next()` itself (and from `iterator_step`) are already JS exceptions: propagate them with `?` and don't
close — the spec says an iterator that threw is already finished.

## Async: Rust ⇒ JS

**`async fn`** is the shortest path. It takes owned parameters only (no `self`, no `&str`, no `Agent`); the call
returns a promise at once, and the result goes through `IntoJs` when it settles. `Result::Err` rejects.

```rust,ignore
async fn doubled(p: Promise<f64>) -> Result<f64, Error> {
    Ok(p.await? * 2.0)
}
```

**`Glue::spawn`** is for everything an `async fn` can't do: use `&self` / `&mut self`, read JS arguments with the
`Agent`, or decide synchronously whether to start work. Do the JS-side work first, move only `Send` data into the
future, and end the future with `Glue::settle(value)`:

```rust,ignore
fn later<'gc>(&self, a: &mut Agent<'gc, '_>, ms: f64) -> Result<Value<'gc>, Error> {
    let wait = Duration::from_secs_f64(ms / 1000.0);
    let label = self.label.clone();                         // clone out of self: the future is 'static
    Glue::spawn(a, async move {
        Offload::new(move || std::thread::sleep(wait)).await;   // blocking work on a thread that wakes the waker
        Glue::settle(label)
    })
}
```

`Offload` is the thread-plus-waker future from [Async and promises](async.md#blocking-work-offload-to-a-thread).

**Back into JS when the work is done.** `Glue::settle` only converts a value. The `Settle` a future returns is really
`Box<dyn FnOnce(&mut Agent) -> Result<Value, Error> + Send>`, so you can write it yourself and do any `Agent` work at
that moment — here, call a JS callback whose result becomes the promise's value:

```rust,ignore
fn after<'gc>(a: &mut Agent<'gc, '_>, ms: f64, f: Function) -> Result<Value<'gc>, Error> {
    let wait = Duration::from_secs_f64(ms / 1000.0);
    Glue::spawn(a, async move {
        Offload::new(move || std::thread::sleep(wait)).await;
        let settle: Settle = Box::new(move |a| f.call(a, Value::Undefined, &[]));
        settle                                               // the type annotation is needed
    })
}
```

```js
await timers.after(5, () => 'callback ran')          // 'callback ran'
```

## Async: JS ⇒ Rust

A **`Promise<T>` parameter** turns a JS promise (or any value: it is taken the way `await` takes it, thenables
included) into a Rust future of `Result<T, Error>`. A rejection arrives as `Error::Exception`; returning it with `?`
rejects your own promise with the **same** JS value, so `instanceof` and `message` survive:

```js
await sink.doubled(Promise.resolve(21))                       // 42
try { await sink.doubled(Promise.reject(new TypeError('nope'))) }
catch (e) { e instanceof TypeError && e.message === 'nope' }  // true
```

**Calling a JS async function and awaiting it in Rust:** call it while you still have the `Agent`, convert its
promise with `FromJs`, then await that inside the future:

```rust,ignore
fn call_async<'gc>(a: &mut Agent<'gc, '_>, f: Function, x: f64) -> Result<Value<'gc>, Error> {
    let p = f.call(a, Value::Undefined, &[x.into()])?;
    let p = <Promise<String> as j8::FromJs>::from_js(a, p)?;
    Glue::spawn(a, async move { Glue::settle(p.await.map(|s| format!("rust saw {s}"))) })
}
```

```js
await sink.callAsync(async x => 'js ' + (x + 1), 41)        // 'rust saw js 42'
```

`T` must be `FromJs` and `Send + 'static` (`f64`, `String`, `Vec<…>`, `j8::Json<T>`, `j8::Bytes`, …): the value is
converted on the JS side when the promise settles, and only the Rust result travels to the future.

## Async iterators: Rust ⇒ JS

An async iterator is an object whose `next()` returns a promise of `{ value, done }`. Combine the two halves above:
`next` is a `&mut self` method that updates state synchronously and returns `Glue::spawn(…)`, whose future ends with
`Glue::settle(Step(…))`.

```rust,ignore
struct Ticker { left: u32, every: Duration }

#[j8::class]
impl Ticker {
    #[js(constructor)]
    fn new(count: u32, ms: f64) -> Self {
        Self { left: count, every: Duration::from_secs_f64(ms / 1000.0) }
    }

    fn next<'gc>(&mut self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        let tick = (self.left > 0).then_some(self.left);    // state changes now, in call order
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

Change the state in `next` itself, not in the future: `for await` waits for each promise, but code that calls `next()`
several times without awaiting must still get the items in order.

## Async iterators: JS ⇒ Rust

This is the one direction a Rust future can't drive alone: stepping a JS async iterator means calling `next()` again
after every await, and a future has no `Agent` (rule 2). Let JavaScript drive the loop and give each item to Rust —
a few lines of prelude on a bound class:

```rust,ignore
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

Early exit, `return()` and errors are then handled by `for await` itself. If Rust needs the items as a stream, let
`push` send them into a channel that a Rust future reads.

## Putting it together

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
        rt.eval(SCRIPT, "main.js")?;      // the snippets above, logging into globalThis.out
        rt.run_until_idle()?;             // ⇐ drives every promise and future; without it nothing async settles
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

## Checklist

| Want | Use |
|---|---|
| JS iterates Rust data, small | return `Vec<T>` |
| JS iterates Rust data, lazily | class with `next` (+ `#[js(name = "return")]`) returning `Step<T>`; prelude puts `Iterator.prototype` under it |
| Rust reads a JS iterable, all of it | `Vec<T>` parameter |
| Rust reads a JS iterable, lazily | `Value` + `get_iterator` / `iterator_step`; `iterator_close` on every early stop |
| JS awaits Rust work | `async fn` (owned params) or `Glue::spawn` + `Glue::settle` |
| Rust awaits a JS promise | `Promise<T>` parameter, or `f.call(…)` then `Promise::<T>::from_js` |
| call JS when Rust work finishes | return your own `Settle` closure |
| JS `for await` over Rust | class whose `next` returns `Glue::spawn(… Step …)`; prelude sets `Symbol.asyncIterator` |
| Rust consumes a JS async iterator | let a prelude `for await` push into a bound class |

Pitfalls: names are camelCased (`sum_until` ⇒ `sumUntil`); a `&self` method that calls JS which re-enters the same
object mutably throws a `TypeError`; never keep a `Value` in a struct or move one into a future; a future that never
wakes its waker hangs `run_until_idle`; in an isolate made without an event loop (not by `j8::Runtime`) `Glue::spawn`
throws a `TypeError`.
