# j8debugger

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的调试命令行 `j8dbg`。字节码没有文本形式，所以只处理二进制形式。

```sh
cargo run -p j8debugger -- compile a.js -o a.j8b    # JS ⇒ 二进制形式的字节码（`-` 读标准输入）
cargo run -p j8debugger -- compile -e 'let s = 0; for (let i = 0; i < 10; i++) s += i; s' -o a.j8b
cargo run -p j8debugger -- check a.j8b              # 读回二进制（魔数、指令集、每个字段）
```

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
