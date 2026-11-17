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
