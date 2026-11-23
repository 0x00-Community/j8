#![expect(clippy::needless_pass_by_value, clippy::unnecessary_wraps, reason = "by-value binding parameters and Result returns are the style under test")]
use j8::{Class, Object, Rest};
use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{Agent, CreateParams, Error, Isolate, Source, Value};
use j8platform::DefaultPlatform;

#[derive(j8::Trace)]
struct Counter {
    n: f64,
}

#[j8::class]
impl Counter {
    #[js(constructor)]
    fn new(start: Option<f64>) -> Self {
        Self { n: start.unwrap_or(0.0) }
    }

    fn inc(&mut self, by: f64) -> f64 {
        self.n += by;
        self.n
    }

    #[js(getter)]
    fn value(&self) -> f64 {
        self.n
    }

    #[js(setter, name = "value")]
    fn set_value(&mut self, v: f64) {
        self.n = v;
    }

    fn sum(items: Rest<'_>) -> usize {
        items.0.len()
    }

    fn describe<'scope>(&self, a: &mut Agent<'scope, '_>, label: String) -> Result<Value<'scope>, Error> {
        Ok(a.string(&format!("{label}={}", self.n)).into())
    }
}

struct Calc;

#[j8::object("calc")]
impl Calc {
    fn twice(x: f64) -> f64 {
        x * 2.0
    }

    #[js(getter)]
    fn answer() -> f64 {
        42.0
    }
}

#[j8::function]
fn greet(name: &str) -> String {
    format!("hi {name}")
}

#[test]
fn class_binding() {
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    let executor = Executor::default();
    let cx = isolate.enter(|scope| scope.mc.stash::<j8framework::ContextBrand>(scope.context(&Builtins)));
    isolate.enter(|scope| {
        let context = scope.mc.fetch(&cx);
        let mut a = scope.agent(context, &executor);
        let global = a.context.global;
        Counter::install(&mut a, global);
        Calc::install(&mut a, global);
        greet::install(&mut a, global);
    });
    let src = "const c = new Counter(2); c.inc(3); c.value = c.value * 2; [c.value, c.describe('n'), Counter.sum(1, 2, 3), c instanceof Counter, String(Object.prototype.toString.call(c)), calc.twice(4), calc.answer, greet('j8'), greet.length].join()";
    let r = executor.eval(&mut isolate, &cx, &Source { text: src, file: "t.js" });
    let text = isolate.enter(|scope| {
        let context = scope.mc.fetch(&cx);
        let mut a = scope.agent(context, &executor);
        let v = a.local(&r.expect("the script runs"));
        String::from(&**a.to_string(v).expect("a string"))
    });
    assert_eq!(text, "10,n=10,3,true,[object Counter],8,42,hi j8,1");
}
