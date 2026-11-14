//! Assembly is the inverse of disassembly: a disassembly listing reads back identical field by field.
use r8framework::Source;
use r8virtasm::Module;

#[test]
fn disassembly_assembles_back() {
    let text = r#"
        function* g(a, b = 1, ...r) { let x = 0n; yield a + 0.1; try { eval("x") } finally {} }
        function h(p, q) { "use strict"; return arguments }
        function m(p) { return arguments[0] + p }
        class A extends Error { #p = 1; constructor() { super(); } get v() { return 1 } }
        async function f() { for await (const z of []) {} }
        [..."é\n\"x́"]; -0; NaN; 1e300; x => x; ({ a: 1, ...{} });
    "#;
    let module = Module::try_from(&Source { text, file: "a.js" }).unwrap();
    let listing = module.to_string();
    let back: Module = listing.parse().unwrap();
    assert_eq!(format!("{back:?}"), format!("{module:?}"));
}
