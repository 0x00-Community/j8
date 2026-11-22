use j8executor::Executor;
use j8platform::DefaultPlatform;

j8::runtime! {
    JsonRt {}
}

struct Case {
    script: &'static str,
    expect: &'static str,
}

const CASES: &[Case] = &[
    Case { script: "JSON.stringify({})", expect: "{}" },
    Case { script: "JSON.stringify([])", expect: "[]" },
    Case { script: "JSON.stringify('')", expect: "\"\"" },
    Case { script: "JSON.stringify('a'.repeat(80))", expect: "\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"" },
    Case { script: "JSON.stringify({'': 1})", expect: "{\"\":1}" },
    Case {
        script: r#"JSON.stringify('\u0000\u0008\u000c\n\r\t\u001f\u007f')"#,
        expect: "\"\\u0000\\b\\f\\n\\r\\t\\u001f\u{7f}\"",
    },
    Case {
        script: r#"JSON.stringify('\u2028\u2029é')"#,
        expect: "\"\u{2028}\u{2029}é\"",
    },
    Case {
        script: r#"JSON.stringify({'"': '"', '\\': '\\', '\u0008': '\u0008', '\n': '\n', '\u0001': '\u0001', '\u007f': '\u007f', '\u2028': '\u2028'})"#,
        expect: "{\"\\\"\":\"\\\"\",\"\\\\\":\"\\\\\",\"\\b\":\"\\b\",\"\\n\":\"\\n\",\"\\u0001\":\"\\u0001\",\"\u{7f}\":\"\u{7f}\",\"\u{2028}\":\"\u{2028}\"}",
    },
    Case {
        script: r#"JSON.stringify([{'"':1,'\n':2},{'"':3,'\n':4}])"#,
        expect: "[{\"\\\"\":1,\"\\n\":2},{\"\\\"\":3,\"\\n\":4}]",
    },
    Case {
        script: "JSON.stringify([0,-0,1,-1,9007199254740991,-9007199254740991,9007199254740992,9007199254740993,1e21,1e-7,0.1,123456789012,NaN,Infinity,-Infinity])",
        expect: "[0,0,1,-1,9007199254740991,-9007199254740991,9007199254740992,9007199254740992,1e+21,1e-7,0.1,123456789012,null,null,null]",
    },
    Case {
        script: "JSON.stringify({n:0,z:-0,f:0.1,e:1e21,s:1e-7,b:123456789012})",
        expect: "{\"n\":0,\"z\":0,\"f\":0.1,\"e\":1e+21,\"s\":1e-7,\"b\":123456789012}",
    },
    Case {
        script: "JSON.stringify({a:1,2:2,b:3,10:4,'01':5,'1':6})",
        expect: "{\"a\":1,\"2\":2,\"b\":3,\"10\":4,\"01\":5,\"1\":6}",
    },
    Case { script: "JSON.stringify({10:1,2:2,a:3})", expect: "{\"10\":1,\"2\":2,\"a\":3}" },
    Case { script: "JSON.stringify({a:1,[Symbol('s')]:2,b:3})", expect: "{\"a\":1,\"b\":3}" },
    Case {
        script: "(() => { const o = {a:1}; Object.defineProperty(o, 'b', {value:2, enumerable:false}); o.c = 3; return JSON.stringify(o); })()",
        expect: "{\"a\":1,\"c\":3}",
    },
    Case { script: "JSON.stringify(Object.assign(Object.create(null), {a:1, b:2}))", expect: "{\"a\":1,\"b\":2}" },
    Case { script: "JSON.stringify([1,,3])", expect: "[1,null,3]" },
    Case { script: "JSON.stringify([undefined, function(){}, 1, Symbol('s')])", expect: "[null,null,1,null]" },
    Case { script: "JSON.stringify({a:undefined, b:function(){}, c:1, d:Symbol('s')})", expect: "{\"c\":1}" },
    Case { script: "JSON.stringify([true, false, null])", expect: "[true,false,null]" },
    Case {
        script: "(() => { const a = [1,2]; a.extra = 3; a.foo = 'z'; return JSON.stringify(a); })()",
        expect: "[1,2]",
    },
    Case { script: "JSON.stringify([[], {a:[1]}], null, 0)", expect: "[[],{\"a\":[1]}]" },
    Case {
        script: "JSON.stringify({a:{}, b:[], c:{d:[]}}, null, 2)",
        expect: "{\n  \"a\": {},\n  \"b\": [],\n  \"c\": {\n    \"d\": []\n  }\n}",
    },
    Case {
        script: "JSON.stringify({a:1, b:[2]}, null, 2)",
        expect: "{\n  \"a\": 1,\n  \"b\": [\n    2\n  ]\n}",
    },
    Case { script: "JSON.stringify({a:1}, null, '\\t')", expect: "{\n\t\"a\": 1\n}" },
    Case { script: "JSON.stringify({a:1}, null, 100)", expect: "{\n          \"a\": 1\n}" },
    Case { script: "JSON.stringify({a:1}, null, 0)", expect: "{\"a\":1}" },
    Case { script: "JSON.stringify({a:1}, null, -5)", expect: "{\"a\":1}" },
    Case { script: "JSON.stringify({a:1}, null, 2.9)", expect: "{\n  \"a\": 1\n}" },
    Case { script: "JSON.stringify({a:1}, null, '1234567890XYZ')", expect: "{\n1234567890\"a\": 1\n}" },
    Case { script: "JSON.stringify({a:[]}, null, '')", expect: "{\"a\":[]}" },
    Case { script: "JSON.stringify([[], {}], null, 1)", expect: "[\n [],\n {}\n]" },
    Case {
        script: "(() => { const holder = {a:1, b:2}; const seen = []; const s = JSON.stringify(holder, function(k, v) { seen.push(k + (this === holder ? '=H' : '=W')); return v; }); return s + '|' + seen.join(','); })()",
        expect: "{\"a\":1,\"b\":2}|=W,a=H,b=H",
    },
    Case {
        script: "JSON.stringify({a:1, b:2, c:3, 1:4, '01':5}, ['b', 'a', 'b', 1, '1', 1.5, true, '01'])",
        expect: "{\"b\":2,\"a\":1,\"1\":4,\"01\":5}",
    },
    Case { script: "JSON.stringify([1,2,3], ['0'])", expect: "[1,2,3]" },
    Case {
        script: "(() => { const o = {a:1, b:2, c:3}; Object.defineProperty(o, 'a', {enumerable:true, configurable:true, get() { delete o.b; o.d = 4; return 1; }}); return JSON.stringify(o); })()",
        expect: "{\"a\":1,\"c\":3}",
    },
    Case {
        script: "(() => { const o = {a:1, b:{c:1}}; Object.defineProperty(o, 'a', {enumerable:true, configurable:true, get() { Object.prototype.toJSON = function() { return 5; }; return 1; }}); let r; try { r = JSON.stringify([o, {d:2}]); } finally { delete Object.prototype.toJSON; } return r; })()",
        expect: "[{\"a\":1,\"b\":5},5]",
    },
    Case { script: "JSON.stringify({toJSON(){ return {a:1}; }, b:2})", expect: "{\"a\":1}" },
    Case {
        script: "(() => { const p = {toJSON(){ return 7; }}; return JSON.stringify(Object.create(p, {a:{value:1, enumerable:true}})); })()",
        expect: "7",
    },
    Case {
        script: "(() => { const a = [1,2]; a.toJSON = () => 'Z'; return JSON.stringify(a); })()",
        expect: "\"Z\"",
    },
    Case {
        script: "(() => { Array.prototype.toJSON = function(){ return this.length; }; let r; try { r = JSON.stringify([1,2,3]); } finally { delete Array.prototype.toJSON; } return r; })()",
        expect: "3",
    },
    Case {
        script: "(() => { const o = {a:1,b:2,c:3,d:4,e:5,f:6,g:7,h:8,i:9}; delete o.a; delete o.c; delete o.e; delete o.g; delete o.i; o.z = 1; return JSON.stringify(o); })()",
        expect: "{\"b\":2,\"d\":4,\"f\":6,\"h\":8,\"z\":1}",
    },
    Case {
        script: "(() => { const a = [1,2,3,4]; Object.defineProperty(a, '0', {enumerable:true, configurable:true, get(){ a.length = 2; return 9; }}); return JSON.stringify(a); })()",
        expect: "[9,2,null,null]",
    },
    Case {
        script: "(() => { const o = {}; o.self = o; try { JSON.stringify(o); return 'no'; } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "TypeError: Converting circular structure to JSON",
    },
    Case {
        script: "(() => { const a = []; a.push(a); try { JSON.stringify(a); return 'no'; } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "TypeError: Converting circular structure to JSON",
    },
    Case {
        script: "(() => { try { return JSON.stringify(1n); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "TypeError: Do not know how to serialize a BigInt",
    },
    Case {
        script: "(() => { try { return JSON.stringify({a:1n}); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "TypeError: Do not know how to serialize a BigInt",
    },
    Case {
        script: "(() => { try { return JSON.stringify([1n]); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "TypeError: Do not know how to serialize a BigInt",
    },
    Case { script: "JSON.stringify([Object(1), Object('ab'), Object(true), Object(false), new Number(1.5), new String('x'), new Boolean(false), Object(1n)])", expect: "[{},{},{},{},{},{},{},{}]" },
    Case { script: "JSON.stringify(new Date(0))", expect: "\"1970-01-01T00:00:00.000Z\"" },
    Case { script: "JSON.stringify(new Date(NaN))", expect: "null" },
    Case {
        script: "(JSON.stringify(undefined) === undefined && JSON.stringify(function(){}) === undefined && JSON.stringify(Symbol()) === undefined) ? 'skip-root' : 'bad'",
        expect: "skip-root",
    },
    Case {
        script: "(() => { const calls = []; const p = new Proxy({a:1, b:2}, { get(t, k, r) { calls.push('get:' + String(k)); return Reflect.get(t, k, r); }, ownKeys(t) { calls.push('keys'); return Reflect.ownKeys(t); } }); return JSON.stringify(p) + '|' + calls.join(','); })()",
        expect: "{}|get:toJSON",
    },
    Case { script: "JSON.stringify(JSON.parse('{\"a\":[1,{\"b\":\"c\"}],\"d\":true}'))", expect: "{\"a\":[1,{\"b\":\"c\"}],\"d\":true}" },
    Case {
        script: "(() => { const o = JSON.parse('{\"a\":1,\"b\":2,\"a\":3}'); return JSON.stringify(o) + '|' + Object.keys(o).join(','); })()",
        expect: "{\"a\":3,\"b\":2}|a,b",
    },
    Case {
        script: "(() => { const o = JSON.parse('{\"__proto__\":{\"admin\":true},\"a\":1}'); return JSON.stringify(o) + '|' + (Object.getPrototypeOf(o) === Object.prototype) + '|' + String(o.admin) + '|' + Object.prototype.hasOwnProperty.call(o, '__proto__') + '|' + Object.keys(o).join(','); })()",
        expect: "{\"__proto__\":{\"admin\":true},\"a\":1}|true|undefined|true|__proto__,a",
    },
    Case {
        script: "(() => { const o = JSON.parse('{\"1\":\"a\",\"0\":\"b\",\"2\":\"c\",\"01\":\"d\"}'); return JSON.stringify(o) + '|' + Object.keys(o).join(','); })()",
        expect: "{\"1\":\"a\",\"0\":\"b\",\"2\":\"c\",\"01\":\"d\"}|0,1,2,01",
    },
    Case {
        script: "(() => { const a = JSON.parse('-0'); const b = JSON.parse('-0.0'); const c = JSON.parse('0'); return (Object.is(a, -0) && Object.is(b, -0) && Object.is(c, 0) && JSON.stringify(a) === '0') ? 'zeros' : 'bad-zeros'; })()",
        expect: "zeros",
    },
    Case {
        script: "JSON.stringify(JSON.parse('[0,-0,1E5,1e-5,1e21,0.1,123456789012,999999999999999,9007199254740991,9007199254740993,12345678901234567890]'))",
        expect: "[0,0,100000,0.00001,1e+21,0.1,123456789012,999999999999999,9007199254740991,9007199254740992,12345678901234567000]",
    },
    Case {
        script: r#"JSON.stringify(JSON.parse('"\\"\\\\\\/\\b\\f\\n\\r\\t\\u0041\\uD83D\\uDE00"'))"#,
        expect: "\"\\\"\\\\/\\b\\f\\n\\r\\tA😀\"",
    },
    Case {
        script: r#"JSON.stringify(JSON.parse('"\\uD800"'))"#,
        expect: "\"\u{FFFD}\"",
    },
    Case {
        script: "(() => { const o = JSON.parse('{\"a\":1,\"b\":2,\"c\":{\"d\":4,\"e\":5}}', function(k, v) { if (k === 'b') return undefined; if (k === 'd') return 9; if (k === 'e') return undefined; return v; }); return JSON.stringify(o) + '|' + Object.keys(o.c).join(','); })()",
        expect: "{\"a\":1,\"c\":{\"d\":9}}|d",
    },
    Case {
        script: "(() => { let s = ''; for (let i = 0; i < 9; i++) s += JSON.stringify(JSON.parse('{\"k' + i + '\":' + i + ',\"z\":1}')); return s + '|' + JSON.stringify(JSON.parse('{\"k0\":0,\"z\":1}')); })()",
        expect: "{\"k0\":0,\"z\":1}{\"k1\":1,\"z\":1}{\"k2\":2,\"z\":1}{\"k3\":3,\"z\":1}{\"k4\":4,\"z\":1}{\"k5\":5,\"z\":1}{\"k6\":6,\"z\":1}{\"k7\":7,\"z\":1}{\"k8\":8,\"z\":1}|{\"k0\":0,\"z\":1}",
    },
    Case {
        script: "(() => { let ok = true; for (let i = 0; i < 40; i++) { const o = JSON.parse('{\"id\":' + i + ',\"name\":\"n\",\"tags\":[\"a\",\"b\",\"c\"],\"nested\":{\"v\":' + i + '}}'); ok = ok && o.id === i && o.name === 'n' && o.tags.join() === 'a,b,c' && o.nested.v === i && Object.keys(o).join() === 'id,name,tags,nested'; } return ok ? 'repeat' : 'bad-repeat'; })()",
        expect: "repeat",
    },
    Case {
        script: "(() => { const a = []; for (let i = 0; i < 100; i++) a.push(i); return JSON.stringify(a) === '[' + a.map(x => JSON.stringify(x)).join(',') + ']' ? 'small-numbers' : 'bad'; })()",
        expect: "small-numbers",
    },
    Case {
        script: "(() => { const a = []; for (let i = 0; i < 100; i++) a.push('s' + i); return JSON.stringify(a) === '[' + a.map(x => JSON.stringify(x)).join(',') + ']' ? 'small-strings' : 'bad'; })()",
        expect: "small-strings",
    },
    Case {
        script: "(() => { const a = []; for (let i = 0; i < 100000; i++) a.push(i); return JSON.stringify(a) === '[' + a.map(x => JSON.stringify(x)).join(',') + ']' ? 'numbers' : 'bad-numbers'; })()",
        expect: "numbers",
    },
    Case {
        script: "(() => { const a = []; for (let i = 0; i < 100000; i++) a.push('s' + i); return JSON.stringify(a) === '[' + a.map(x => JSON.stringify(x)).join(',') + ']' ? 'strings' : 'bad-strings'; })()",
        expect: "strings",
    },
    Case { script: "(() => { const a = [1]; a[3] = 4; return JSON.stringify(a); })()", expect: "[1,null,null,4]" },
    Case { script: "JSON.parse('   1  ')", expect: "1" },
    Case { script: "JSON.parse('true')", expect: "true" },
    Case { script: "JSON.parse('false')", expect: "false" },
    Case { script: "JSON.parse('null')", expect: "null" },
    Case {
        script: "(() => { try { JSON.parse(''); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected end of JSON input in JSON at position 0 (expected a value)",
    },
    Case {
        script: "(() => { try { JSON.parse('1 2'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected token '2' in JSON at position 2 (unexpected trailing input)",
    },
    Case {
        script: "(() => { try { JSON.parse('[1,]'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected token ']' in JSON at position 3 (expected a value)",
    },
    Case {
        script: "(() => { try { JSON.parse('{\"a\":1,}'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected token '}' in JSON at position 7 (expected a property name)",
    },
    Case {
        script: "(() => { try { JSON.parse('{\"a\" 1}'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected token '1' in JSON at position 5 (expected ':')",
    },
    Case {
        script: "(() => { try { JSON.parse('{a:1}'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected token 'a' in JSON at position 1 (expected a property name)",
    },
    Case {
        script: "(() => { try { JSON.parse('-'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected end of JSON input in JSON at position 1 (bad number)",
    },
    Case {
        script: "(() => { try { JSON.parse('01'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected token '1' in JSON at position 1 (unexpected trailing input)",
    },
    Case {
        script: "(() => { try { JSON.parse('1.'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected end of JSON input in JSON at position 2 (bad number)",
    },
    Case {
        script: "(() => { try { JSON.parse('1e'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected end of JSON input in JSON at position 2 (bad number)",
    },
    Case {
        script: "(() => { try { JSON.parse('1e+'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected end of JSON input in JSON at position 3 (bad number)",
    },
    Case {
        script: "(() => { try { JSON.parse('\"hi'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected end of JSON input in JSON at position 3 (unterminated string)",
    },
    Case {
        script: "(() => { try { JSON.parse('\"' + '\\u0001' + '\"'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected token '\u{1}' in JSON at position 1 (bad control character in string)",
    },
    Case {
        script: "(() => { try { JSON.parse('\"\\\\q\"'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected token 'q' in JSON at position 2 (bad escape)",
    },
    Case {
        script: "(() => { try { JSON.parse('\"\\\\u12\"'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected token '1' in JSON at position 3 (bad \\u escape)",
    },
    Case {
        script: "(() => { try { JSON.parse('\"\\\\uXXXX\"'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected token 'X' in JSON at position 3 (bad \\u escape)",
    },
    Case {
        script: "(() => { try { JSON.parse('['); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected end of JSON input in JSON at position 1 (expected a value)",
    },
    Case {
        script: "(() => { try { JSON.parse('{'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected end of JSON input in JSON at position 1 (expected a property name)",
    },
    Case {
        script: "(() => { try { JSON.parse('[1,2'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected end of JSON input in JSON at position 4 (expected ',' or ']')",
    },
    Case {
        script: "(() => { try { JSON.parse('{\"a\":1'); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected end of JSON input in JSON at position 6 (expected ',' or '}')",
    },
    Case {
        script: "(() => { try { JSON.parse(\"'\"); } catch (e) { return e.name + ': ' + e.message; } })()",
        expect: "SyntaxError: Unexpected token ''' in JSON at position 0 (expected a value)",
    },
];

#[test]
fn json_fast_paths_keep_the_language_result() {
    let platform = JsonRt::platform(DefaultPlatform::default());
    j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, |set| {
        let rt = JsonRt::runtime(set, Executor::default()).expect("runtime");
        for (i, case) in CASES.iter().enumerate() {
            let got = rt.eval(case.script, "json.js").unwrap_or_else(|e| panic!("case {i} threw {e}"));
            assert_eq!(rt.text(&got), case.expect, "case {i}");
        }
    });
}
