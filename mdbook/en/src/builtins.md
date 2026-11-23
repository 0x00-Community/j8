# Built-in objects

`crates/j8builtins` holds JavaScript's standard built-in objects and implements `j8framework::Intrinsics`: when a
`Realm` is created, `Builtins::install` puts in the global object, the global functions and every built-in constructor,
prototype and method.

```rust,ignore
let cx = isolate.enter(|scope| scope.mc.stash::<RealmBrand>(scope.context(&j8builtins::Builtins).realm()));
```

`j8framework` only defines `Intrinsics`; this is the default implementation, and hosts can replace or trim it.
j8builtins **depends only on `j8framework`**, not on the executor: every built-in is a host function
`fn(&mut Agent, this, &[Value]) -> Result<Value, Error>` written with the semantics on `Agent`, calling back into JS
through `Invoke`.

## Files

| File | Built-ins |
|---|---|
| `global.rs` | the global object, `parseInt` / `parseFloat` / `isNaN` / `eval` / the URI functions |
| `object.rs`, `function.rs`, `reflect.rs` | `Object`, `Function`, `Reflect` / `Proxy` |
| `array.rs`, `typed.rs`, `buffer.rs` | `Array`, TypedArrays (12 kinds, Float16 included), `ArrayBuffer` / `SharedArrayBuffer` / `DataView` / `Atomics` |
| `string.rs`, `text.rs` | `String`, `Symbol` |
| `regexp.rs`, `dfa.rs` | `RegExp` ([regress](https://crates.io/crates/regress) plus a DFA fast path) |
| `json.rs` | `JSON` |
| `math.rs`, `bigint.rs`, `date.rs` | `Math`, `BigInt`, `Date` |
| `collections.rs`, `weak.rs` | `Map` / `Set`, `WeakMap` / `WeakSet` / `WeakRef` / `FinalizationRegistry` |
| `promise.rs`, `asyncgen.rs`, `iter.rs` | `Promise`, async generators, iterators and iterator helpers |
| `error.rs` | `Error`, `TypeError` … `AggregateError`, `cause` |
| `helpers.rs` | shared spec steps |

## Coverage

- **Object / Array**: every method; Array methods generic over array-likes, species; `from` / `flat` / `toSorted` /
  `with` …
- **Reflection**: `Reflect`, `Proxy` (revocable too);
- **Iterator helpers**: the global `Iterator`, `Iterator.from`, `map` / `filter` / `take` / `drop` / `flatMap` /
  `reduce` …; Set methods (`union` …), `Map.groupBy` / `Object.groupBy`;
- **Binary data**: resizable `ArrayBuffer` (`transfer`, detach), growable `SharedArrayBuffer`, `DataView`, `Atomics`
  (single-threaded semantics);
- **Date** (the local time zone is UTC); **number formatting** (`toFixed` / `toPrecision` / `toExponential` with exact
  decimals), `Math.sumPrecise` / `f16round`;
- **RegExp**: the Symbol protocol, `lastIndex`, the `d` flag, named groups, species, `RegExp.escape`; the string methods
  `match` / `matchAll` / `replace` / `replaceAll` / `search` / `split`;
- **Promise** built on NewPromiseCapability: subclassing, `all` / `race` / `allSettled` / `any` / `withResolvers` / `try`;
- built-in constructors take the prototype from `new.target`, so `class X extends Map` works; host functions tell
  constructible from not.

Out of scope: Temporal, intl402, ShadowRealm and others (see the [Roadmap](roadmap.md)).

## Regexps: two engines

- The main engine is regress, a backtracking engine matching UTF-16 code units;
- `dfa.rs` is a second one: for patterns the `regex` crate matches exactly like ECMAScript on an all-ASCII subject, it
  uses finite automata with a literal prefilter instead of backtracking. `Dfa::new` translates the pattern and refuses
  everything whose meaning could differ: back references, lookaround, named groups, `v` mode, anchors under `m`, capture
  groups inside a quantifier (ECMAScript resets them every iteration), Annex B oddities, non-ASCII pattern text. regress
  handles the rest and non-ASCII subjects.
- Regex literals share the realm's compiled entry and strings; `test()` without `g` / `y` skips `lastIndex`.

## Fast paths

Hot built-ins have fast paths that skip the full spec steps **when no observable behavior could differ**: an untouched
`RegExp.prototype` (the pristine check), appending to the end of a dense array, string-and-number concatenation, JSON
parsing repeated key sequences into one shape. test262 is the judge of correctness.

Pure computation over large data (sort keys, number conversions) can be split into chunks on the platform's thread pool
through `j8framework::Par`, with the results written back afterwards.
