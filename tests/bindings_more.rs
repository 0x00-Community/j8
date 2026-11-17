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
