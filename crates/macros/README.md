# j8macros

**English** · [简体中文](README.zh-CN.md)

Procedural macros of [j8](https://github.com/0x00-Community/j8).

| Macro | What it generates |
| --- | --- |
| `enums!("x.json")` | Enums (definitions, `Display`, assembly, field accessors) from a JSON file under the calling crate; the JSON is the single source of truth (used for `j8virtasm`'s instruction set) |
| `#[heap]` | j8's own heap structs: `#[mutable]` fields kept in the cell `j8framework::Mutable` picks (`ValueCell`, `Slot`, atomics) with generated getters / setters |
| `#[derive(Collect)]` | paragc's `Trace` for heap types; `#[collect(require_static)]` skips a field (or, on the type, says it has no GC edges); types with edges may not implement `Drop` |
| `#[class]` / `#[object]` / `#[function]` / `#[module]` | Rust ⇄ JS bindings, used through the facade crate as `#[j8::class]` etc.; `#[j8::module("node:fs")]` exports a native module for `import` / `require`, and `j8::runtime!` (in the facade crate) assembles globals, modules and a prelude into a runtime |

See [USAGE.md](USAGE.md) for the JSON format and every binding attribute.

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
