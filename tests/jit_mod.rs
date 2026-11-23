#![cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]

use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{CreateParams, Isolate, Source};
use j8platform::DefaultPlatform;

#[derive(Clone, Copy)]
enum Tier {
    Interpreter,
    Immediate,
    Default,
}

impl Tier {
    fn label(self) -> &'static str {
        match self {
            Self::Interpreter => "interpreter",
            Self::Immediate => "threshold 0",
            Self::Default => "default threshold",
        }
    }
}

struct Case {
    what: &'static str,
    text: &'static str,
    want: &'static str,
}

const PAIRS: &str = r#"function show(x) {
  if (Object.is(x, -0)) return '-0';
  if (Object.is(x, 0)) return '0';
  if (Number.isNaN(x)) return 'NaN';
  return String(x);
}
function loop(a, b) {
  let r = 0;
  for (let i = 0; i < 80; i++) r = a % b;
  return show(r);
}
const pairs = [
  [4, 2], [-4, 2], [4, -2], [-4, -2],
  [5, 2], [-5, 2], [5, -2], [-5, -2],
  [1, 2], [-1, 2], [0, 1], [-0, 1],
  [1, 0], [1, -0], [-1, 0], [0, 0],
  [NaN, 1], [1, NaN], [NaN, NaN],
  [Infinity, 1], [-Infinity, 1], [1, Infinity], [-1, Infinity], [1, -Infinity], [Infinity, Infinity], [-Infinity, Infinity],
  [4.5, 2], [-4.5, 2], [4.5, -2], [-4.5, -2], [5.25, 2.5], [-5.25, 2.5],
  [1, 1], [-1, 1], [1, -1], [-1, -1], [8, 1], [-8, 1], [8, -1], [-8, -1],
  [3, 5], [-3, 5], [3, -5], [-3, -5],
  [2 ** 53 - 1, 10], [2 ** 53, 3], [2 ** 53 + 2, 10], [-(2 ** 53), 3], [-(2 ** 53 - 1), 10], [-(2 ** 53 + 2), 10],
  [2 ** 62 - 1024, 1000], [2 ** 62, 2147483648], [2 ** 62, 3], [2 ** 62 + 1024, 1000], [-(2 ** 62), 2147483648], [-(2 ** 62 - 1024), 1000], [-(2 ** 62 + 1024), 7],
  [2 ** 63 - 2048, 10], [2 ** 63, 3], [2 ** 63 + 2048, 10], [-(2 ** 63), 3], [-(2 ** 63 - 2048), 10], [-(2 ** 63 + 2048), 10],
  [2 ** 100, 7], [-(2 ** 100), 7], [2 ** 1023, 2 ** 100], [-(2 ** 1023), 2 ** 100],
  [1 / 3, 0.2], [-1 / 3, 0.2], [Math.PI, 1], [-Math.PI, 2],
  [2147483647, 3], [-2147483648, 3], [2147483648, 3],
];
const out = pairs.map(([a, b]) => loop(a, b));
function m(a, b) {
  let r = 0;
  for (let i = 0; i < 8; i++) r = a % b;
  return r;
}
out.push(show(m(2 ** 53, 7)));
out.push(show(m(2 ** 62, 2147483648)));
out.push(show(m(-4, 2)));
out.push(show(m(-4.5, 2)));
let seed = 7;
for (let i = 0; i < 100000; i++) seed = (seed * 1103515245 + 12345) % 2147483648;
out.push(show(seed));
out.join('|')
"#;

const CASES: &[Case] = &[
    Case {
        what: "remainder of integral and non-integral doubles, both signs, across 2^53, 2^62 and 2^63",
        text: PAIRS,
        want: "0|-0|0|-0|1|-1|1|-1|1|-1|0|-0|NaN|NaN|NaN|NaN|NaN|NaN|NaN|NaN|NaN|1|-1|1|NaN|NaN|0.5|-0.5|0.5|-0.5|0.25|-0.25|0|-0|0|-0|0|-0|0|-0|3|-3|3|-3|1|2|4|-2|-1|-4|880|0|1|928|-0|-880|-6|0|2|6|-2|-0|-6|2|-2|0|-0|0.1333333333333333|-0.1333333333333333|0.14159265358979312|-1.1415926535897931|1|-2|2|4|0|-0|-0.5|1193434624",
    },
    Case {
        what: "a remainder site sees ints first and a double past 2^53 later",
        text: "function m(a, b) { return a % b; } let s = ''; for (let i = 0; i < 30; i++) { const r = m(i, 3); s = Object.is(r, -0) ? '-0' : String(r); } const big = m(2 ** 62, 2147483648); const neg = m(-4, 2); s + ':' + big + ':' + (Object.is(neg, -0) ? '-0' : String(neg))",
        want: "2:0:-0",
    },
    Case {
        what: "divisors at and beyond 2^62 and 2^63, where the conversion to an integer saturates",
        text: "function show(x) { if (Object.is(x, -0)) return '-0'; if (Number.isNaN(x)) return 'NaN'; return String(x); } function loop(a, b) { let r = 0; for (let i = 0; i < 80; i++) r = a % b; return show(r); } const pairs = [[5, 2 ** 63], [5, -(2 ** 63)], [-5, 2 ** 63], [2 ** 62, 2 ** 63], [-(2 ** 62), 2 ** 63], [2 ** 62, -(2 ** 63)], [2 ** 61 + 2048, 2 ** 63 + 2048], [5, 2 ** 62 + 1024], [2 ** 62, 2 ** 62 + 1024], [2 ** 62, 2 ** 62 - 1024], [-(2 ** 62), 2 ** 62 - 1024], [2 ** 62, 2 ** 62], [-(2 ** 62), 2 ** 62], [7, 2 ** 64], [2 ** 62, 2 ** 100], [2 ** 62 - 1024, 2 ** 63 - 2048], [2 ** 60 + 1024, 2 ** 53 + 2], [2 ** 62, 2 ** 53 - 1], [-(2 ** 62), -(2 ** 53 - 1)], [2 ** 62, 1], [2 ** 62, -1], [-(2 ** 62), -1], [2 ** 62, 0.5], [2 ** 62, 1.5], [-(2 ** 62), 2 ** 63 - 1024], [2 ** 62 - 512, -(2 ** 63 + 2048)]]; pairs.map(([a, b]) => loop(a, b)).join('|')",
        want: "5|5|-5|4611686018427388000|-4611686018427388000|4611686018427388000|2305843009213696000|5|4611686018427388000|1024|-1024|0|-0|7|4611686018427388000|4611686018427387000|768|512|-512|0|0|-0|0|1|-4611686018427388000|4611686018427387400",
    },
];

fn completion(text: &str, tier: Tier) -> String {
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    let executor = match tier {
        Tier::Interpreter => Executor::default(),
        Tier::Immediate => Executor::default().with_tier(j8jit::Jit::from(0)),
        Tier::Default => Executor::default().with_tier(j8jit::Jit::default()),
    };
    let cx = isolate.enter(|scope| scope.mc.stash::<j8framework::RealmBrand>(scope.context(&Builtins).realm()));
    let r = executor.eval(&mut isolate, &cx, &Source { text, file: "jit_mod.js" }).unwrap_or_else(|e| panic!("script failed ({}): {e:?}\n{text}", tier.label()));
    isolate.enter(|scope| {
        let realm = scope.mc.fetch(&cx).into();
        let mut a = scope.agent(realm, &executor);
        let v = a.local(&r);
        String::from(&**a.to_string(v).unwrap_or_else(|e| panic!("ToString failed ({}): {e:?}\n{text}", tier.label())))
    })
}

fn run_cases() {
    for case in CASES {
        for tier in [Tier::Interpreter, Tier::Immediate, Tier::Default] {
            assert_eq!(completion(case.text, tier), case.want, "{} ({})", case.what, tier.label());
        }
    }
}

#[test]
fn compiled_remainder_matches_the_interpreter() {
    let ran = std::thread::Builder::new().stack_size(16 << 20).spawn(run_cases).expect("a thread with room for the compiled frames");
    ran.join().expect("the cases finish");
}
