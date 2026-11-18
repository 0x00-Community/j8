# j8debugger

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的调试命令行 `j8dbg`。

```sh
cargo run -p j8debugger -- disasm a.js              # JS ⇒ 字节码，打印反汇编清单（`-` 读标准输入）
cargo run -p j8debugger -- disasm -e 'let s = 0; for (let i = 0; i < 10; i++) s += i; s'
cargo run -p j8debugger -- asm a.j8s                # 汇编清单，打印规范化后的清单（`-o` 写文件）
```

清单是 `j8bytecode` 的文本形式（汇编与反汇编互为逆）。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
