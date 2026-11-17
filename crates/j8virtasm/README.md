# j8virtasm

**English** · [简体中文](README.zh-CN.md)

The virtual instruction set of [j8](https://github.com/0x00-Community/j8): an accumulator-style register bytecode
(like V8's Ignition), plus the JavaScript ⇒ bytecode compiler.

- The instruction enum is generated from `insn.json` by `j8macros::enums!` (the JSON is the single source of truth)
- `Insn` / `Function` / `Module`; assembling and disassembling are exact inverses:
  `text.parse::<Module>()` / `module.to_string()`
- `Module::try_from(&Source)` compiles JavaScript (parsed with [oxc](https://oxc.rs)); there is no AST interpreter,
  everything (including `eval` and `Function`) goes through bytecode

## Usage

```rust
let module = j8virtasm::Module::try_from(&source)?;
println!("{module}");                       // the disassembly listing
let again: j8virtasm::Module = module.to_string().parse()?;
```

The `j8dbg` tool (`debugger/`) prints and assembles listings from the command line.

## Features

| Feature | Default | Meaning |
| --- | --- | --- |
| `compile` | on | The JS ⇒ bytecode compiler (oxc); without it only the bytecode types remain |

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
