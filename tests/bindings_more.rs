//! Binding extensions: `#[j8::class(extends = Base)]`, `j8::Function`, `j8::Bytes`, `j8::Persistent`.
use j8::{Bytes, Function, Persistent};
use j8executor::Executor;
use j8framework::{Agent, Error, Value};
use j8platform::DefaultPlatform;

struct Animal {
    name: String,
}

#[j8::class]
impl Animal {
    #[js(constructor)]
    fn new(name: String) -> Self {
        Self { name }
    }

    fn describe(&self) -> String {
        format!("{} the animal", self.name)
    }

    fn rename(&mut self, name: String) {
        self.name = name;
    }
}

struct Dog {
    base: Animal,
    tricks: u32,
}

impl AsRef<Animal> for Dog {
    fn as_ref(&self) -> &Animal {
        &self.base
    }
}

impl AsMut<Animal> for Dog {
    fn as_mut(&mut self) -> &mut Animal {
        &mut self.base
    }
}

#[j8::class(extends = Animal)]
impl Dog {
    #[js(constructor)]
    fn new(name: String) -> Self {
        Self { base: Animal { name }, tricks: 0 }
    }

    fn learn(&mut self) -> u32 {
        self.tricks += 1;
        self.tricks
    }
}

/// Keeps a callback and a value across calls
#[derive(Default)]
struct Hooks {
    on: Option<Function>,
    kept: Option<Persistent>,
}

#[j8::class]
impl Hooks {
    #[js(constructor)]
    fn new() -> Self {
        Self::default()
    }

    fn set(&mut self, f: Function, keep: Persistent) {
        self.on = Some(f);
        self.kept = Some(keep);
    }

    fn fire<'gc>(&self, a: &mut Agent<'gc, '_>, x: f64) -> Result<Value<'gc>, Error> {
        let kept = self.kept.as_ref().map_or(Value::Undefined, |k| k.get(a));
        match &self.on {
            Some(f) => f.call(a, Value::Undefined, &[x.into(), kept]),
            None => Ok(Value::Undefined),
        }
    }
}

struct Bin;

#[j8::object("bin")]
impl Bin {
    fn reverse(b: Bytes) -> Bytes {
        Bytes(b.0.into_iter().rev().collect())
    }
}

j8::runtime! {
    Zoo {
        globals: [Animal, Dog, Hooks, Bin],
    }
}

#[test]
fn extends_functions_bytes_persistent() {
    let platform = Zoo::platform(DefaultPlatform::default());
    let mut rt = Zoo::runtime(&platform, Executor::default()).expect("runtime");
    let src = "
        const d = new Dog('rex');
        d.rename('max');
        const inherit = [d instanceof Dog, d instanceof Animal, Object.getPrototypeOf(Dog) === Animal, d.describe(), d.learn(), d.learn()].join();
        let wrong;
        try { Dog.prototype.learn.call(new Animal('cat')); } catch (e) { wrong = e instanceof TypeError; }
        const h = new Hooks();
        h.set((x, k) => x * 2 + k.n, { n: 1 });
        const bytes = Array.from(bin.reverse(new Uint8Array([1, 2, 3]).buffer)).join();
        [inherit, wrong, h.fire(20), bytes].join('|');
    ";
    let r = rt.eval(src, "t.js").expect("runs");
    assert_eq!(rt.text(&r), "true,true,true,max the animal,1,2|true|41|3,2,1");
}

/// A future that is ready after a thread sleeps `ms` and wakes it
struct Sleep {
    started: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ms: u64,
}

impl std::future::Future for Sleep {
    type Output = ();

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        match &self.started {
            Some(done) if done.load(std::sync::atomic::Ordering::Acquire) => std::task::Poll::Ready(()),
            Some(_) => std::task::Poll::Pending,
            None => {
                let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let (flag, waker, ms) = (done.clone(), cx.waker().clone(), self.ms);
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    flag.store(true, std::sync::atomic::Ordering::Release);
                    waker.wake();
                });
                self.started = Some(done);
                std::task::Poll::Pending
            }
        }
    }
}

struct Later;

#[j8::object("later")]
impl Later {
    #[expect(clippy::unused_async_trait_impl, reason = "an async binding that is ready at once is part of what is tested")]
    async fn double(x: f64) -> f64 {
        x * 2.0
    }

    async fn after(ms: f64, text: String) -> String {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a small test delay")]
        Sleep { started: None, ms: ms as u64 }.await;
        format!("{text} after {ms}ms")
    }
}

j8::runtime! {
    Loop {
        globals: [Later],
    }
}

#[test]
fn async_bindings_settle_on_the_event_loop() {
    let platform = Loop::platform(DefaultPlatform::default());
    let mut rt = Loop::runtime(&platform, Executor::default()).expect("runtime");
    rt.eval("globalThis.log = []; later.after(20, 'b').then(v => log.push(v)); later.double(21).then(v => log.push(v)); log.push(later.double(1) instanceof Promise);", "t.js").expect("runs");
    rt.run_until_idle().expect("settles");
    let r = rt.eval("log.join()", "check.js").expect("runs");
    assert_eq!(rt.text(&r), "true,42,b after 20ms");
    // Without an event loop (a plain platform), the call throws a TypeError
    let plain = DefaultPlatform::default();
    let mut bare = j8::Runtime::new::<Loop>(&plain, Executor::default()).expect("runtime");
    let r = bare.eval("try { later.double(1); 'no' } catch (e) { e instanceof TypeError }", "t.js").expect("runs");
    assert_eq!(bare.text(&r), "true");
}
