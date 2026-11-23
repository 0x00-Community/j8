use j8::{Bytes, Function};
use j8executor::Executor;
use j8framework::{Agent, Error, Value};
use j8platform::DefaultPlatform;

#[derive(j8::Trace)]
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

#[derive(j8::Trace)]
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

#[derive(Default, j8::Trace)]
struct Hooks<'scope> {
    on: Option<Function<'scope>>,
    kept: Option<Value<'scope>>,
}

#[j8::class]
impl<'scope> Hooks<'scope> {
    #[js(constructor)]
    fn new() -> Self {
        Self::default()
    }

    fn set(&mut self, f: Function<'scope>, keep: Value<'scope>) {
        self.on = Some(f);
        self.kept = Some(keep);
    }

    fn fire(&self, a: &mut Agent<'scope, '_>, x: f64) -> Result<Value<'scope>, Error> {
        let kept = self.kept.unwrap_or(Value::Undefined);
        match self.on {
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
    j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, |set| {
        let rt = Zoo::runtime(set, Executor::default()).expect("runtime");
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
    });
}

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
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl, reason = "an async binding that is ready at once is part of what is tested")]
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
    j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, |set| {
        let rt = Loop::runtime(set, Executor::default()).expect("runtime");
        rt.eval("globalThis.log = []; later.after(20, 'b').then(v => log.push(v)); later.double(21).then(v => log.push(v)); log.push(later.double(1) instanceof Promise);", "t.js").expect("runs");
        rt.run_until_idle().expect("settles");
        let r = rt.eval("log.join()", "check.js").expect("runs");
        assert_eq!(rt.text(&r), "true,42,b after 20ms");
    });
    let plain = DefaultPlatform::default();
    j8::EventLoopSet::scope(&plain, std::num::NonZeroUsize::MIN, |set| {
        let rt = j8::Runtime::on::<Loop>(set, Executor::default()).expect("runtime");
        rt.eval("globalThis.got = 0; later.double(4).then(v => got = v);", "t.js").expect("runs");
        rt.run_until_idle().expect("settles");
        let r = rt.eval("got", "check.js").expect("runs");
        assert_eq!(rt.text(&r), "8");
        let mut isolate = j8framework::Isolate::new(&plain, j8framework::CreateParams::default());
        let executor = Executor::default();
        let cx = isolate.enter(|scope| scope.mc.stash::<j8framework::RealmBrand>(scope.context(&j8builtins::Builtins).realm()));
        isolate.enter(|scope| {
            let realm = scope.mc.fetch(&cx).into();
            let mut a = scope.agent(realm, &executor);
            <Loop as j8::Host>::install(&mut a);
        });
        let src = j8framework::Source { text: "try { later.double(1); 'no' } catch (e) { e instanceof TypeError }", file: "t.js" };
        let r = executor.eval(&mut isolate, &cx, &src).expect("runs");
        let text = isolate.enter(|scope| {
            let realm = scope.mc.fetch(&cx).into();
            let mut a = scope.agent(realm, &executor);
            let v = a.local(&r);
            a.to_string(v).map(|s| String::from(&**s)).expect("ToString")
        });
        assert_eq!(text, "true");
    });
}

fn eval_text<'p>(isolate: &mut j8framework::Isolate<'p>, executor: &Executor<'p>, cx: &j8framework::ScopeGlobal<'p, j8framework::RealmBrand>, text: &str) -> String {
    let r = executor.eval(isolate, cx, &j8framework::Source { text, file: "t.js" }).expect("runs");
    isolate.enter(|scope| {
        let realm = scope.mc.fetch(cx).into();
        let mut a = scope.agent(realm, executor);
        let v = a.local(&r);
        a.to_string(v).map(|s| String::from(&**s)).expect("ToString")
    })
}

#[test]
fn host_fields_keep_their_objects_across_collections() {
    let platform = DefaultPlatform::default();
    let mut isolate = j8framework::Isolate::new(&platform, j8framework::CreateParams::default());
    let executor = Executor::default();
    let cx = isolate.enter(|scope| scope.mc.stash::<j8framework::RealmBrand>(scope.context(&j8builtins::Builtins).realm()));
    isolate.enter(|scope| {
        let realm = scope.mc.fetch(&cx).into();
        let mut a = scope.agent(realm, &executor);
        <Zoo as j8::Host>::install(&mut a);
    });
    eval_text(&mut isolate, &executor, &cx, "globalThis.h = new Hooks(); h.set((x, k) => x * 2 + k.n, { n: 1 }); 0");
    for _ in 0..3 {
        eval_text(&mut isolate, &executor, &cx, "for (let i = 0; i < 10000; i++) ({ i }); 0");
        isolate.collect();
    }
    assert_eq!(eval_text(&mut isolate, &executor, &cx, "h.fire(20)"), "41");
}

#[cfg(feature = "json")]
mod json {
    use j8::Json;
    use j8executor::Executor;
    use j8platform::DefaultPlatform;

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Point {
        x: f64,
        y: f64,
        tags: Vec<String>,
    }

    struct Geo;

    #[j8::object("geo")]
    impl Geo {
        fn mirror(p: Json<Point>) -> Json<Point> {
            let Point { x, y, mut tags } = p.0;
            tags.push(String::from("mirrored"));
            Json(Point { x: -x, y: -y, tags })
        }
    }

    j8::runtime! {
        Maps {
            globals: [Geo],
        }
    }

    #[test]
    fn json_values_cross_through_serde() {
        let platform = Maps::platform(DefaultPlatform::default());
        j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, |set| {
            let rt = Maps::runtime(set, Executor::default()).expect("runtime");
            let r = rt.eval("const m = geo.mirror({ x: 1, y: -2, tags: ['a'] }); let bad; try { geo.mirror({ x: 'no' }); } catch (e) { bad = e instanceof TypeError; } [m.x, m.y, m.tags.join('+'), bad].join()", "t.js").expect("runs");
            assert_eq!(rt.text(&r), "-1,2,a+mirrored,true");
        });
    }
}
