# j8debugger

**English** · [简体中文](README.zh-CN.md)

`j8dbg`, the debugging command line of [j8](https://github.com/0x00-Community/j8).

```sh
cargo run -p j8debugger -- disasm a.js              # JS ⇒ bytecode, print the disassembly listing (`-` reads stdin)
cargo run -p j8debugger -- disasm -e 'let s = 0; for (let i = 0; i < 10; i++) s += i; s'
cargo run -p j8debugger -- asm a.j8s                # assemble a listing, print the normalized listing (`-o` writes a file)
```

Listings use the text form of `j8bytecode` (assembling and disassembling are exact inverses).

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
