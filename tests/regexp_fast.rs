use j8executor::Executor;
use j8platform::DefaultPlatform;

j8::runtime! {
    Bare {}
}

fn eval(script: &str) -> String {
    let platform = Bare::platform(DefaultPlatform::default());
    j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, |set| {
        let rt = Bare::runtime(set, Executor::default()).expect("runtime");
        match rt.eval(script, "regexp_fast.js") {
            Ok(v) => rt.text(&v),
            Err(e) => format!("error:{e}"),
        }
    })
}

const CASES: &[(&str, &str)] = &[
    (
        "let out = []; for (let i = 0; i < 3; i++) { let r = /ab/g; r.lastIndex = i; out.push(r.lastIndex); } let x = /ab/g; let y = /ab/i; x.lastIndex = 8; out.push(x !== y, x.lastIndex, y.lastIndex, x.flags, y.flags, x.source); function f() { return /ab/g; } function g() { return /ab/g; } let p = f(), q = g(); p.lastIndex = 4; out.push(p !== q, q.lastIndex, f() !== p); out.join('|')",
        "0|1|2|true|8|0|g|i|ab|true|0|true",
    ),
    (
        "[/a/.source, /bb/.test('xbb'), /c/i.test('C'), /d/g.flags, /e/m.flags, /f/s.flags, /g/y.flags, /h/u.flags, /i/d.flags, /j/v.flags].join('|')",
        "a|true|true|g|m|s|y|u|d|v",
    ),
    (
        "let n = 0; for (let i = 0; i < 200; i++) { if (/\\d+/.test('a1') && /o/g.test('o')) n++; } n",
        "200",
    ),
    (
        "const text = 'The quick brown fox 123 jumps over the lazy dog 4567 '; let c = 0; for (let i = 0; i < 20; i++) { if (/\\d+/.test(text)) c++; c += text.replace(/o/g, '0').length; } c + '|' + text.replace(/o/g, '0')",
        "1080|The quick br0wn f0x 123 jumps 0ver the lazy d0g 4567 ",
    ),
    (
        "const text = 'The quick brown fox 123 jumps over the lazy dog 4567 '; const re = /\\d+/; const ro = /o/g; let c = 0; for (let i = 0; i < 20; i++) { if (re.test(text)) c++; c += text.replace(ro, '0').length; } [c, ro.lastIndex, re.lastIndex].join('|')",
        "1080|0|0",
    ),
    (
        "let r = new RegExp('\\\\d+', 'g'); let q = RegExp('a', 'g'); [r.test('a1b'), r.lastIndex, r.source, r.flags, 'aa'.replace(q, 'b'), q.lastIndex, new RegExp('a', 'i').test('A')].join('|')",
        "true|2|\\d+|g|bb|0|true",
    ),
    (
        "let r = /a/g; let s = 'aba'; [r.test(s), r.lastIndex, r.test(s), r.lastIndex, r.test(s), r.lastIndex].join('|')",
        "true|1|true|3|false|0",
    ),
    (
        "let r = /a/y; let a = r.test('xa'); let i1 = r.lastIndex; r.lastIndex = 1; let b = r.test('xa'); let i2 = r.lastIndex; let c = /a/y; let d = c.test('aa'); [a, i1, b, i2, d, c.lastIndex].join('|')",
        "false|0|true|2|true|1",
    ),
    (
        "let a = /a/g; a.lastIndex = NaN; let b = /a/g; b.lastIndex = Infinity; let c = /a/g; c.lastIndex = -1; let d = /a/g; d.lastIndex = 2.9; [a.test('a'), a.lastIndex, b.test('a'), b.lastIndex, c.test('ba'), c.lastIndex, d.test('abaca'), d.lastIndex].join('|')",
        "true|1|false|0|true|2|true|3",
    ),
    (
        "let r = /a/g; let n = 0; r.lastIndex = { valueOf() { n++; return 1; } }; let t = r.test('aba'); [t, n, r.lastIndex].join('|')",
        "true|1|3",
    ),
    (
        "let r = /a/g; let order = []; r.lastIndex = { valueOf() { order.push('li'); return 0; } }; let arg = { toString() { order.push('s'); return 'a'; } }; r.test(arg); order.join(',')",
        "s,li",
    ),
    (
        "function grab(fn) { try { fn(); return 'no'; } catch (e) { return e.name + ':' + e.message; } } let r = /a/g; Object.freeze(r); let t = grab(() => r.test('a')); let u = grab(() => 'a'.replace(r, 'b')); let q = /a/; Object.freeze(q); [t, u, q.test('a')].join('|')",
        "TypeError:Cannot assign to read only property 'lastIndex'|TypeError:Cannot assign to read only property 'lastIndex'|true",
    ),
    (
        "RegExp.prototype.exec = function() { return null; }; let r = /a/g; [r.test('a'), 'a'.replace(r, 'b'), r.lastIndex].join('|')",
        "false|a|0",
    ),
    (
        "let n = 0; Object.defineProperty(RegExp.prototype, 'global', { configurable: true, get() { n++; return false; } }); let r = /a/g; let a = r.test('aa'); let i1 = r.lastIndex; let b = r.test('aa'); let i2 = r.lastIndex; let s = 'aa'.replace(/a/g, 'b'); [a, i1, b, i2, s, n > 0].join('|')",
        "true|1|true|2|ba|true",
    ),
    (
        "RegExp.prototype[Symbol.replace] = function() { return 'X'; }; let r = /\\d/; [r.test('1'), 'ab'.replace(r, 'Q')].join('|')",
        "true|X",
    ),
    (
        "let r = /a/; r.exec = function() { return null; }; String(r.test('a'))",
        "false",
    ),
    (
        "let r = /a/g; r[Symbol('q')] = 1; [r.test('a'), 'aa'.replace(r, 'b')].join('|')",
        "true|bb",
    ),
    (
        "class R extends RegExp {} let r = new R('a', 'g'); [r.test('ab'), r.lastIndex, 'aa'.replace(r, 'b'), r.source, r.flags].join('|')",
        "true|1|bb|a|g",
    ),
    (
        "let r = /a/g; let proto = Object.create(RegExp.prototype); proto.exec = function() { return null; }; Object.setPrototypeOf(r, proto); [String(r.test('a')), 'aa'.replace(r, 'b')].join('|')",
        "false|aa",
    ),
    (
        "let r = /a/; r.compile('a', 'g'); let first = r.test('aa'); let idx = r.lastIndex; let rep = 'aa'.replace(r, 'b'); r.compile(/b/i); [first, idx, rep, r.source, r.flags, r.global, r.ignoreCase, r.test('B'), 'B'.replace(r, 'x')].join('|')",
        "true|1|bb|b|i|false|true|true|x",
    ),
    (
        "let s = 'abc'; let t = s.replace(/z/g, '0'); [t === s, t === 'abc', 'ab'.replace(/(?:)/g, '-'), 'abx'.replace(/x*/g, '-'), 'abc'.replace(/x*/g, '-'), 'aba'.replace(/^a|a$/g, 'X')].join('|')",
        "true|true|-a-b-|-a-b--|-a-b-c-|XbX",
    ),
    (
        "['ab'.replace(/b/g, 'BBB'), 'ab'.replace(/b/g, ''), 'ooo'.replace(/o/g, '0'), 'aba'.replace(/a/g, 'X'), 'hi'.replace(/i/, '!'), 'abcdefghijklmnopqrstuvwxyz'.replace(/z/g, '0123456789')].join('|')",
        "aBBB|a|000|XbX|h!|abcdefghijklmnopqrstuvwxy0123456789",
    ),
    (
        "['ab'.replace(/(a)/, \"[$1][$&][$`][$'][$$]\"), 'ab'.replace(/(?<x>a)(?<y>b)/, '{$<y>}{$<x>}{$<z>}'), 'ab'.replace(/(a)/, '$<x>'), 'z'.replace(/z/, '$$')].join('|')",
        "[a][a][][b][$]b|{b}{a}{}|$<x>b|$",
    ),
    (
        "let r = /a/g; let seen = []; let out = 'aba'.replace(r, function(m) { seen.push(r.lastIndex + m); return 'x'; }); let q = /a/; q.lastIndex = 4; let seen2 = []; 'a'.replace(q, function() { seen2.push(q.lastIndex); return 'z'; }); [out, seen.join(','), r.lastIndex, seen2.join(','), q.lastIndex].join('|')",
        "xbx|0a,0a|0|4|4",
    ),
    (
        "['\\u00e9\\u00e9'.replace(/\\u00e9/g, 'e'), 'ab'.replace(/a/, '\\u00e9'), '\\u{1F600}'.replace(/./gu, 'x'), '\\u{1F600}'.replace(/./g, 'x'), '\\u{1F600}'.replace(/(?:)/gu, '-'), 'a\\u{1F600}b'.replace(/\\u{1F600}/gu, 'x')].join('|')",
        "ee|\u{e9}b|x|xx|-\u{1F600}-|axb",
    ),
    (
        "'\\u{1F600}'.replace(/(?:)/g, '-')",
        "-\u{FFFD}-\u{FFFD}-",
    ),
    (
        "let a = 'hi'.replace(/i/, '!'); let b = 'abcdefghijklmnopqrstuvwxyz'.replace(/z/g, '0123456789'); [a.length, a.charCodeAt(1), a[0], a.codePointAt(0), b.length, b.charCodeAt(34), b[0], b.codePointAt(26)].join('|')",
        "2|33|h|104|35|57|a|49",
    ),
    (
        "['aaa'.replace('a', 'b'), 'aaa'.replaceAll('a', 'b'), 'aaa'.replaceAll('aa', 'b'), 'aaaa'.replaceAll('aa', 'b'), 'abc'.replaceAll('', '-'), 'abc'.replace('', '-'), 'abc'.replaceAll('', ''), 'abc'.replace('z', 'y')].join('|')",
        "baa|bbb|ba|bb|-a-b-c-|-abc|abc|abc",
    ),
    (
        "['ab'.replace('a', '$$'), 'ab'.replace('a', '$&'), 'ab'.replace('b', '$`'), 'ab'.replace('a', \"$'\"), 'ab'.replace('a', '$1'), 'abc'.replaceAll('b', '$&$&'), '\\u00e9\\u00e9'.replace('\\u00e9', 'e'), '\\u00e9\\u00e9'.replaceAll('\\u00e9', 'e'), 'ab'.replace('a', '\\u00e9'), 'a\\u00e9b'.replaceAll('\\u00e9', 'e')].join('|')",
        "$b|ab|aa|bb|$1b|abbc|e\u{e9}|ee|\u{e9}b|aeb",
    ),
    (
        "let a = 'hello'; let b = '\\u00e9'; let c = '\\u{1F600}'; let d = 'ab'.replace(/b/, 'c'); let e = 'ab'.replace(/b/, '\\u00e9'); let f = '\\u00e9' + 'a'; let g = 'a' + '\\u{1F600}'; [a.length, a.charCodeAt(1), a[1], a.codePointAt(1), b.length, b.charCodeAt(0), b[0], b.codePointAt(0), c.length, c.charCodeAt(0), c.charCodeAt(1), c.codePointAt(0), d.length, d.charCodeAt(1), d[0], d.codePointAt(0), e.length, e.charCodeAt(1), e.codePointAt(1), f.length, f.charCodeAt(0), f[1], f.codePointAt(0), g.length, g.charCodeAt(1), g.codePointAt(1)].join('|')",
        "5|101|e|101|1|233|\u{e9}|233|2|55357|56832|128512|2|99|a|97|2|233|233|2|233|a|233|3|55357|128512",
    ),
    (
        "['a-b-c'.split(/-/).join(','), 'a-b-c'.split(/-/, 2).join(','), 'aba'.match(/a/g).join(','), 'a1b2'.match(/\\d/g).join(','), 'aba'.match(/a/)[0]].join('|')",
        "a,b,c|a,b|a,a|1,2|a",
    ),
    (
        "let r = /a/g; Object.defineProperty(r, 'global', { value: false }); let m = 'aa'.match(r); m.length + ':' + m[0]",
        "1:a",
    ),
    (
        "let r = /a/y; r.lastIndex = 1; let s = 'ba'.replace(r, 'X'); let g = /a/gy; let t = 'aaba'.replace(g, '0'); let h = /a/gy; let u = 'ba'.replace(h, 'X'); [s, r.lastIndex, t, g.lastIndex, u, h.lastIndex].join('|')",
        "bX|2|00ba|0|ba|0",
    ),
    (
        "let r = /a/; r.lastIndex = 5; let s = 'a'.replace(r, 'b'); let g = /a/g; g.lastIndex = 5; let t = 'a'.replace(g, 'b'); [s, r.lastIndex, t, g.lastIndex].join('|')",
        "b|5|b|0",
    ),
    (
        "['AbC'.replace(/b/gi, 'x'), 'AbC'.replace(/b/i, 'x'), 'a\\nb'.replace(/^b/m, 'x'), 'a\\nb'.replace(/a.b/s, 'x'), 'ab'.replace(/a/d, 'x')].join('|')",
        "AxC|AxC|a\nx|x|xb",
    ),
    (
        "const show = v => v === null ? 'null' : v.join(','); ['abc'.match(/z/g), 'abc'.match(/z/), 'aba'.match(/a/g), 'a'.match(/^a|a$/g), 'aa'.match(/a/g), 'ab'.match(/(?:)/g), 'ab'.match(/x*/g), 'abx'.match(/x*/g), 'a b'.match(/\\b/g)].map(show).join('|')",
        "null|null|a,a|a|a,a|,,|,,|,,x,|,,,",
    ),
    (
        "let m = 'ab'.match(/(a)(b)/); let n = 'ab'.match(/(a)|(b)/); let p = 'ab'.match(/(?<x>a)(?<y>b)/); let q = 'ab'.match(/(a)/d); [m[0], m[1], m[2], m.index, m.input, String(m.groups), n[0], n[1], String(n[2]), p.groups.x, p.groups.y, String(p.groups.z), q.indices[0].join('-'), q.indices[1].join('-'), String(q.indices.groups), 'ab'.match(/(?<x>a)/g).join(',')].join('|')",
        "ab|a|b|0|ab|undefined|a|a|undefined|a|b|undefined|0-1|0-1|undefined|a",
    ),
    (
        "let r = /a/g; r.lastIndex = 5; let m = 'aba'.match(r); let y = /a/y; y.lastIndex = 1; let t = 'ba'.match(y); let g = /a/gy; g.lastIndex = 1; let u = 'ba'.match(g); let p = /a/; p.lastIndex = 4; let v = 'aba'.match(p); [m.join(','), r.lastIndex, t[0], t.index, y.lastIndex, String(u), g.lastIndex, v[0], v.index, p.lastIndex].join('|')",
        "a,a|0|a|1|2|null|0|a|0|4",
    ),
    (
        "let r = /\\u{1F600}/uy; r.lastIndex = 1; let m = 'a\\u{1F600}'.match(r); [m[0], m.index, r.lastIndex, 'a\\u{1F600}'.search(/\\u{1F600}/u), 'a\\u{1F600}b'.match(/\\u{1F600}/gu).join(','), '\\u{1F600}'.match(/(?:)/gu).join(','), '\\u{1F600}'.match(/(?:)/g).join(','), '\\u{00e9}-\\u{00e9}'.match(/\\u{00e9}/g).join(','), 'a\\u{00e9}a'.match(/\\u{00e9}/).index].join('|')",
        "\u{1F600}|1|3|1|\u{1F600}|,|,,|\u{e9},\u{e9}|1",
    ),
    (
        "['abc'.search(/z/), 'abc'.search(/a/), 'abc'.search(/c/), ''.search(/(?:)/), ''.search(/a/), 'a\\u{00e9}a'.search(/\\u{00e9}/), '\\u{1F600}'.search(/./u), '\\u{1F600}'.search(/./)].join('|')",
        "-1|0|2|0|-1|1|0|0",
    ),
    (
        "let r = /a/g; r.lastIndex = 5; let a = 'ba'.search(r); let y = /a/y; y.lastIndex = 3; let b = 'ba'.search(y); let c = 'ab'.search(y); let d = /a/y; d.lastIndex = 1; let e = 'ba'.search(d); [a, r.lastIndex, b, y.lastIndex, c, d.lastIndex, e].join('|')",
        "1|5|-1|3|0|1|-1",
    ),
    (
        "const show = v => v.map(x => x === undefined ? 'u' : x).join(','); [show('abc'.split(/z/)), show('a'.split(/a/)), show('-a-'.split(/-/)), show('a--b'.split(/-/)), show('ab'.split(/(?:)/)), show('ab'.split(/x*/)), show('abx'.split(/x*/)), show('a b'.split(/\\b/)), show('ab'.split(/(a)/)), show('a'.split(/(b)|(a)/)), show('ab'.split(/(?<x>a)/)), show('ab'.split(/(a)(x)?/))].join('|')",
        "abc|,|,a,|a,,b|a,b|a,b|a,b,|a, ,b|,a,b|,u,a,|,a,b|,a,u,b",
    ),
    (
        "const show = v => v.map(x => x === undefined ? 'u' : x).join(','); [show('x'.split(/$/)), show('x'.split(/^/)), show('abc'.split(/\\b/)), show('x'.split(/.?/)), show('x'.split(/()/)), show('ab'.split(/a*?/)), show('ab'.split(/a*/))].join('|')",
        "x|x|abc|,|x|a,b|,b",
    ),
    (
        "const show = v => v.map(x => x === undefined ? 'u' : x).join(','); let lim = [0, 1, 2, undefined, -1, 2 ** 32 + 1, true, false, '2'].map(n => show('a-b-c'.split(/-/, n))); let e = [''.split(/a/).length, ''.split(/(?:)/).length, ''.split(/a*/).length, ''.split(/\\b/).length]; let r = /a/y; r.lastIndex = 2; let s = 'a-a'.split(r); let g = /a/g; g.lastIndex = 4; let t = 'a-a'.split(g); let h = /a/gy; h.lastIndex = 9; let u = 'a-a'.split(h); [lim.join(';'), e.join(','), show(s), r.lastIndex, show(t), g.lastIndex, show(u), h.lastIndex, show('ab'.split(/(a)/, 1)), show('ab'.split(/(a)/, 2))].join('|')",
        ";a;a,b;a,b,c;a,b,c;a;a;;a,b|1,0,0,1|,-,|2|,-,|4|,-,|9||,a",
    ),
    (
        "const codes = s => { let o = []; for (let i = 0; i < s.length; i++) o.push(s.charCodeAt(i)); return o.join('.'); }; const parts = a => a.map(x => x === undefined ? 'u' : codes(String(x))).join(','); [parts('\\u{1F600}'.split(/(?:)/u)), parts('\\u{1F600}'.split(/(?:)/)), parts('a\\u{1F600}b'.split(/(?:)/u)), 'a\\u{1F600}b'.split(/\\u{1F600}/u).join(','), '\\u{00e9}-\\u{00e9}'.split(/-/).join(','), parts('\\u{00e9}'.split(/x*/)), 'a\\u{00e9}a'.search(/\\u{00e9}/)].join('|')",
        "55357.56832|65533,65533|97,55357.56832,98|a,b|\u{e9},\u{e9}|233|1",
    ),
    (
        "[new String('a-b').split(/-/).join(','), new String('aba').match(/a/g).join(','), new String('abc').search(/b/), String.prototype.match.call(1, /1/), String.prototype.split.call(12, /1/).join(',')].join('|')",
        "a,b|a,a|1|1|,2",
    ),
    (
        "let r = /a/g; r.lastIndex = 3; r.exec = function() { return null; }; let m = 'aa'.match(r); let se = 'ba'.search(r); let sp = 'a-a'.split(r); [String(m), se, sp.join(','), r.lastIndex].join('|')",
        "null|-1|,-,|0",
    ),
    (
        "RegExp.prototype.exec = function() { return null; }; let r = /a/g; r.lastIndex = 3; [String('aa'.match(r)), 'ba'.search(/a/), 'a-a'.split(/-/).join(','), r.lastIndex].join('|')",
        "null|-1|a-a|0",
    ),
    (
        "let n = 0; Object.defineProperty(RegExp.prototype, 'global', { configurable: true, get() { n++; return false; } }); let r = /a/g; let m = 'aa'.match(r); let sp = 'a-a'.split(r); let se = 'ba'.search(r); [m[0], m.index, m.length, r.lastIndex, sp.join(','), se, n].join('|')",
        "a|0|1|1|,-,|1|2",
    ),
    (
        "let n = 0; Object.defineProperty(RegExp.prototype, 'hasIndices', { configurable: true, get() { n++; return false; } }); let m = 'ab'.match(/a/d); let se = 'ab'.search(/b/); [m[0], m.indices ? 'ind' : 'no', se, n].join('|')",
        "a|ind|1|1",
    ),
    (
        "let n = 0; Object.defineProperty(RegExp.prototype, 'unicode', { configurable: true, get() { n++; return false; } }); let s = '\\u{1F600}'.split(/(?:)/u); let codes = s.map(p => { let c = []; for (let i = 0; i < p.length; i++) c.push(p.charCodeAt(i)); return c.join('.'); }); [codes.join(','), n, '\\u{1F600}'.match(/(?:)/gu).length].join('|')",
        "65533,65533|1|3",
    ),
    (
        "RegExp.prototype[Symbol.split] = function() { return 'X'; }; ['a-b'.split(/-/), 'ab'.match(/a/g).join(','), 'ab'.search(/b/)].join('|')",
        "X|a|1",
    ),
    (
        "delete RegExp.prototype[Symbol.split]; let t; try { t = 'a-b'.split(/-/).join(','); } catch (e) { t = e.name; } let m = 'ab'.match(/a/g).join(','); [t, m].join('|')",
        "a-b|a",
    ),
    (
        "delete RegExp.prototype[Symbol.match]; let t; try { 'ab'.match(/a/); t = 'no'; } catch (e) { t = e.name + ':' + e.message; } [t, 'a-b'.split(/-/).join(',')].join('|')",
        "TypeError:undefined is not a function|a,b",
    ),
    (
        "RegExp.prototype[Symbol.search] = function() { return 9; }; ['ab'.search(/b/), 'ab'.match(/a/)[0]].join('|')",
        "9|a",
    ),
    (
        "delete RegExp.prototype[Symbol.search]; let t; try { 'ab'.search(/b/); t = 'no'; } catch (e) { t = e.name + ':' + e.message; } t",
        "TypeError:undefined is not a function",
    ),
    (
        "let n = 0; RegExp.prototype[Symbol.matchAll] = function() { n++; return { next() { return { done: true }; } }; }; 'ab'.matchAll(/a/g); String(n)",
        "1",
    ),
    (
        "let r = /a/g; r[Symbol('q')] = 1; r.lastIndex = 5; let se = 'ba'.search(r); let after = r.lastIndex; let m = 'aa'.match(r); let sp = 'a-a'.split(r); [se, after, m.join(','), sp.join(','), r.lastIndex].join('|')",
        "1|5|a,a|,-,|0",
    ),
    (
        "function C() {} C[Symbol.species] = function() { return /z/y; }; let r = /a/; r.constructor = C; r.lastIndex = 6; [ 'a-b'.split(r).join(','), r.lastIndex, 'ab'.match(/a/g).join(',') ].join('|')",
        "a-b|6|a",
    ),
    (
        "function C() {} C[Symbol.species] = function() { return /z/y; }; RegExp.prototype.constructor = C; ['a-b'.split(/a/).join(','), 'ab'.match(/a/g).join(','), 'ab'.search(/b/)].join('|')",
        "a-b|a|1",
    ),
    (
        "let n = 0; Object.defineProperty(RegExp, Symbol.species, { configurable: true, value: function() { n++; return /z/y; } }); ['a-b'.split(/a/).join(','), n, 'aa'.match(/a/g).join(',')].join('|')",
        "a-b|1|a,a",
    ),
    (
        "let n = 0; class R extends RegExp { constructor(p, f) { n++; super(p, f); } } let r = new R('a', 'g'); r.lastIndex = 3; let m = 'aa'.match(r); let sp = 'a-a'.split(r); let se = 'ba'.search(r); [m.join(','), sp.join(','), se, n, r.lastIndex].join('|')",
        "a,a|,-,|1|2|0",
    ),
    (
        "const grab = fn => { try { return 'ok:' + String(fn()); } catch (e) { return e.name; } }; let r = /a/g; Object.freeze(r); let q = /a/; Object.freeze(q); [grab(() => 'aa'.match(r)), grab(() => 'ab'.search(q)), grab(() => 'a-b'.split(q).join(',')), grab(() => 'ab'.search(r)), q.lastIndex].join('|')",
        "TypeError|ok:0|ok:,-b|TypeError|0",
    ),
    (
        "let n = 0; let r = /a/g; r.lastIndex = 4; let lim = { valueOf() { n++; r.compile('z', 'g'); return 2; } }; let s = 'a-b-a'.split(r, lim); [s.join(','), n, r.lastIndex, r.source].join('|')",
        ",-b-|1|0|z",
    ),
];

#[test]
fn regexp_fast_paths() {
    for (script, expected) in CASES {
        assert_eq!(eval(script), *expected, "{script}");
    }
}

#[test]
fn regexp_literals_in_two_realms() {
    let platform = Bare::platform(DefaultPlatform::default());
    j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, |set| {
        let a = Bare::runtime(set, Executor::default()).expect("realm a");
        let b = Bare::runtime(set, Executor::default()).expect("realm b");
        let script = "let r = /ab/g; r.lastIndex = 3; [r.lastIndex, r.source, r.flags, /ab/i.flags, /ab/g !== r].join('|')";
        let va = a.eval(script, "a.js").expect("realm a script");
        let vb = b.eval(script, "b.js").expect("realm b script");
        assert_eq!(a.text(&va), "3|ab|g|i|true");
        assert_eq!(b.text(&vb), "3|ab|g|i|true");
    });
}
