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
    j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, |set| {
        let rt = Child::runtime(set, Executor::default()).expect("preludes run");
        rt.eval_module("import calc, { add } from 'calc'; import again from 'demo:calc'; globalThis.sum = add(1, 2) + calc.add(3, 4) + (again === calc ? 100 : 0);", "main.mjs").expect("module runs");
        let r = rt.eval("[sum, layers.join(), greeter.hello('j8'), typeof __j8_native.greeter.hello, Object.keys(globalThis).includes('__j8_native')].join('|')", "check.js").expect("script runs");
        assert_eq!(rt.text(&r), "110|base,child|hello j8|function|false");
    });
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
    j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, |set| {
        let one = Async::runtime(set, Executor::default()).expect("runtime");
        let two = Async::runtime(set, Executor::default()).expect("runtime");
        one.eval("later.echo(1).then(v => globalThis.got = v)", "one.js").expect("script runs");
        two.eval("later.echo(2).then(v => globalThis.got = v)", "two.js").expect("script runs");
        one.run_until_idle().expect("loop runs");
        two.run_until_idle().expect("loop runs");
        let (a, b) = (one.eval("got", "a.js").expect("got"), two.eval("got", "b.js").expect("got"));
        assert_eq!((one.text(&a), two.text(&b)), ("1".to_owned(), "2".to_owned()));
    });
}

#[test]
fn promise_handles_await_js_promises() {
    let platform = Async::platform(DefaultPlatform::default());
    j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, |set| {
        let rt = Async::runtime(set, Executor::default()).expect("runtime");
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
    });
}

#[test]
fn rejections_keep_their_own_reasons() {
    let platform = Async::platform(DefaultPlatform::default());
    j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, |set| {
        let rt = Async::runtime(set, Executor::default()).expect("runtime");
        let src = "
            const out = [];
            let rejectA, rejectB;
            const a = new Promise((_, r) => rejectA = r);
            const b = new Promise((_, r) => rejectB = r);
            later.double(a).catch(e => out.push('a:' + e));
            later.double(b).catch(e => out.push('b:' + e));
            later.echo(0).then(() => { rejectB('second'); rejectA('first'); });
            let after = 'unset';
            later.echo(0).then(() => { try { after = 'clean'; } catch (e) { after = 'stale ' + e; } });
        ";
        rt.eval(src, "main.js").expect("script runs");
        rt.run_until_idle().expect("loop runs");
        let r = rt.eval("out.sort().join('|') + '|' + after", "check.js").expect("runs");
        assert_eq!(rt.text(&r), "a:first|b:second|clean");
    });
}

struct Refusing(DefaultPlatform);

impl j8::frame::Pool for Refusing {
    fn threads(&self) -> usize {
        self.0.threads()
    }

    fn run(&self, jobs: Vec<j8::frame::Task<'_>>) {
        self.0.run(jobs);
    }
}

impl j8::frame::Platform for Refusing {
    fn now(&self) -> std::time::Duration {
        self.0.now()
    }

    fn load_module(&self, scope: &mut j8::frame::Scope<'_, '_>, referrer: &str, specifier: &str) -> Result<(String, String), j8::frame::Error> {
        match specifier {
            "./refused.mjs" => Err(scope.throw(j8::frame::ErrorKind::Range, &format!("{referrer} may not import {specifier}"))),
            "./ok.mjs" => Ok((String::from("ok.mjs"), String::from("export const ok = 1;"))),
            _ => Err(scope.module_not_found(referrer, specifier)),
        }
    }

    fn print(&self, line: &str) {
        self.0.print(line);
    }
}

#[test]
fn a_failed_module_load_throws_the_hosts_error() {
    let platform = Base::platform(Refusing(DefaultPlatform::default()));
    j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, |set| {
        let rt = Base::runtime(set, Executor::default()).expect("runtime");
        assert!(rt.eval_module("import './refused.mjs';", "main.mjs").is_err(), "a refused static import fails the module");
        assert!(rt.exception_text().starts_with("RangeError: main.mjs may not import ./refused.mjs"), "{}", rt.exception_text());
        rt.eval("globalThis.seen = []; for (const s of ['./refused.mjs', './missing.mjs', './ok.mjs']) import(s).then((m) => seen.push(m.ok), (e) => seen.push(`${e.name}: ${e.message}`));", "dynamic.js").expect("script runs");
        rt.run_until_idle().expect("loop runs");
        let r = rt.eval("seen.join('|')", "check.js").expect("script runs");
        assert_eq!(rt.text(&r), "RangeError: dynamic.js may not import ./refused.mjs|TypeError: Cannot find module './missing.mjs' imported from 'dynamic.js'|1");
    });
}
