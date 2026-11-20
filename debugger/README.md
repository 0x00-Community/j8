# j8debugger

**English** · [简体中文](README.zh-CN.md)

`j8dbg`, the debugging command line of [j8](https://github.com/0x00-Community/j8). Bytecode has no text form, so it
works on the binary form only.

```sh
cargo run -p j8debugger -- compile a.js -o a.j8b    # JS ⇒ bytecode in binary form (`-` reads stdin)
cargo run -p j8debugger -- compile -e 'let s = 0; for (let i = 0; i < 10; i++) s += i; s' -o a.j8b
cargo run -p j8debugger -- check a.j8b              # read the binary back (magic, instruction set, every field)
```

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
