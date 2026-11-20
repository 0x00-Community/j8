# j8bcdbg

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的字节码调试命令行 `j8bcdbg`：编译 JS，按函数列出字节码，格式照 V8 的
`--print-bytecode`。自成一包、不在 j8 的 workspace 里，按版本依赖发布的 `j8`。

```sh
cargo run -- compile a.js                 # `-` 读标准输入
cargo run -- compile -e 'let s = 0; for (let i = 0; i < 10; i++) s += i; s'
cargo run -- compile a.js -o a.txt        # 写到文件，缺省标准输出
```

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
