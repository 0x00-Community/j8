# j8bcdbg

**English** · [简体中文](README.zh-CN.md)

`j8bcdbg`, the bytecode debugging command line of [j8](https://github.com/0x00-Community/j8): compiles JS and lists
the bytecode per function, the way V8's `--print-bytecode` does. It is a package of its own, outside the j8 workspace,
and depends on the published `j8` by version.

```sh
cargo run -- compile a.js                 # `-` reads stdin
cargo run -- compile -e 'let s = 0; for (let i = 0; i < 10; i++) s += i; s'
cargo run -- compile a.js -o a.txt        # write to a file instead of stdout
```

## License

MIT, as part of [j8](https://github.com/0x00-Community/j8).
