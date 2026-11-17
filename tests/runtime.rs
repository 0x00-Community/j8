//! `j8::runtime!`: globals, native modules (`import` and the registry), hidden bindings, preludes, a runtime built on
//! top of another.
use j8executor::Executor;
use j8platform::DefaultPlatform;

struct Calc;

#[j8::module("demo:calc", "calc")]
impl Calc {
    fn add(a: f64, b: f64) -> f64 {
        a + b
    }
}

struct Greeter;

#[j8::object("greeter")]
impl Greeter {
    fn hello(name: &str) -> String {
        format!("hello {name}")
    }
}

j8::runtime! {
    Base {
        globals: [Greeter],
        modules: [Calc],
        prelude: "globalThis.layers = ['base'];",
    }
}

j8::runtime! {
    Child: Base {
        natives: [Greeter],
        prelude: "layers.push('child');",
    }
}

#[test]
fn runtime_layers_modules_and_preludes() {
    let platform = Child::platform(DefaultPlatform::default());
    let mut rt = Child::runtime(&platform, Executor::default()).expect("preludes run");
    rt.eval_module("import calc, { add } from 'calc'; import again from 'demo:calc'; globalThis.sum = add(1, 2) + calc.add(3, 4) + (again === calc ? 100 : 0);", "main.mjs").expect("module runs");
    let r = rt.eval("[sum, layers.join(), greeter.hello('j8'), typeof __j8_native.greeter.hello, Object.keys(globalThis).includes('__j8_native')].join('|')", "check.js").expect("script runs");
    assert_eq!(rt.text(&r), "110|base,child|hello j8|function|false");
}

struct Later;

#[j8::object("later")]
impl Later {
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl, reason = "an async binding that is ready at once is part of what is tested")]
    async fn echo(x: f64) -> f64 {
        x
    }

    async fn double(p: j8::Promise<f64>) -> Result<f64, j8::frame::Error> {
        Ok(p.await? * 2.0)
    }
}

j8::runtime! {
    Async {
        globals: [Later],
    }
}

#[test]
fn runtimes_sharing_a_platform_settle_their_own_work() {
    let platform = Async::platform(DefaultPlatform::default());
    let mut one = Async::runtime(&platform, Executor::default()).expect("runtime");
    let mut two = Async::runtime(&platform, Executor::default()).expect("runtime");
    one.eval("later.echo(1).then(v => globalThis.got = v)", "one.js").expect("script runs");
    two.eval("later.echo(2).then(v => globalThis.got = v)", "two.js").expect("script runs");
    one.run_until_idle().expect("loop runs");
    two.run_until_idle().expect("loop runs");
    let (a, b) = (one.eval("got", "a.js").expect("got"), two.eval("got", "b.js").expect("got"));
    assert_eq!((one.text(&a), two.text(&b)), ("1".to_owned(), "2".to_owned()));
}

#[test]
fn promise_handles_await_js_promises() {
    let platform = Async::platform(DefaultPlatform::default());
    let mut rt = Async::runtime(&platform, Executor::default()).expect("runtime");
    let src = "
        const out = [];
        let resolveLate;
        const late = new Promise(r => resolveLate = r);
        later.double(Promise.resolve(21)).then(v => out.push('resolved ' + v));
        later.double(5).then(v => out.push('plain ' + v));
        later.double(Promise.reject(new Error('no'))).catch(e => out.push('rejected ' + e.message));
        later.double(late).then(v => out.push('late ' + v));
        later.echo(0).then(() => resolveLate(4));
        later.double({ then(f) { f(8); } }).then(v => out.push('thenable ' + v));
    ";
    rt.eval(src, "main.js").expect("script runs");
    rt.run_until_idle().expect("loop runs");
    let r = rt.eval("out.sort().join('|')", "check.js").expect("script runs");
    assert_eq!(rt.text(&r), "late 8|plain 10|rejected no|resolved 42|thenable 16");
}
