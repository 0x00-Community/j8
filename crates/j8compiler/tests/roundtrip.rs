//! The binary form is lossless: compiled bytecode encodes and decodes back identical field by field.
use j8bytecode::Module;
use j8compiler::Compile;
use j8framework::{CodeKind, Source};

#[test]
fn binary_decodes_back() {
    let text = r#"
        function* g(a, b = 1, ...r) { let x = 0n; yield a + 0.1; try { eval("x") } finally {} }
        function h(p, q) { "use strict"; return arguments }
        function m(p) { return arguments[0] + p }
        class A extends Error { #p = 1; constructor() { super(); } get v() { return 1 } }
        async function f() { for await (const z of []) {} }
        [..."é\n\"x́"]; -0; NaN; 1e300; x => x; ({ a: 1, ...{} }); for (let i = 0; i < 3; i++) i < 2;
    "#;
    let module = Module::compile(&Source { text, file: "a.js" }, CodeKind::Script).unwrap();
    let bytes = Vec::<u8>::from(&module);
    assert!(Module::is_binary(&bytes) && !Module::is_binary(text.as_bytes()));
    let back = Module::try_from(bytes.as_slice()).unwrap();
    assert_eq!(format!("{back:?}"), format!("{module:?}"));
    assert_eq!(Module::try_from(&bytes[..bytes.len() - 1]).err(), Some(j8bytecode::BinError::Eof));
}
