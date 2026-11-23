use j8builtins::Builtins;
use j8executor::Executor;
use j8framework::{CreateParams, Error, Isolate, Source};
use j8platform::DefaultPlatform;

const CASES: &[(&str, &str)] = &[
    // 默认比较函数按 ToString 的 UTF-16 码元序排序，不是数值序
    ("[10, 9, 1].sort().join()", "1,10,9"),
    // map 跳过空洞，结果里保留空洞
    ("[1, , 3].map(x => x * 2).join('|')", "2||6"),
    // 显式 String() 允许把 Symbol 转成字符串（对应隐式 ToString 会抛错）
    ("String(Symbol('x'))", "Symbol(x)"),
    ("'' + Symbol('x')", "Uncaught TypeError: Cannot convert a Symbol value to a string"),
];

fn outcome(text: &str) -> String {
    let platform = DefaultPlatform::default();
    let mut isolate = Isolate::new(&platform, CreateParams::default());
    let executor = Executor::default();
    let cx = isolate.enter(|scope| scope.mc.stash::<j8framework::RealmBrand>(scope.context(&Builtins).realm()));
    let r = executor.eval(&mut isolate, &cx, &Source { text, file: "semantics.js" });
    isolate.enter(|scope| {
        let realm = scope.mc.fetch(&cx).into();
        let mut a = scope.agent(realm, &executor);
        match r {
            Ok(g) => {
                let v = a.local(&g);
                String::from(&**a.to_string(v).expect("ToString"))
            }
            Err(Error::Exception) => {
                let v = a.take_exception();
                format!("Uncaught {}", String::from(&**a.to_string(v).expect("ToString")))
            }
            Err(e) => panic!("not a completion: {e}"),
        }
    })
}

#[test]
fn javascript_semantics() {
    let bad: Vec<String> = CASES.iter().filter_map(|(text, want)| {
        let got = outcome(text);
        (got != *want).then(|| format!("{text}\n  want: {want}\n  got:  {got}"))
    }).collect();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
