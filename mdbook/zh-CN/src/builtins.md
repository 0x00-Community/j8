# 内建对象

`crates/j8builtins` 是 JavaScript 标准内建对象，实现 `j8framework::Intrinsics`：新建 `Context` 时由 `Builtins::install`
把全局对象、全局函数和全部内建构造器、原型、方法装进去。

```rust,ignore
let cx = isolate.create_context(&j8builtins::Builtins);
```

`j8framework` 只定义 `Intrinsics` 接口，这里是缺省实现；宿主可以换掉或裁剪。j8builtins **只依赖 `j8framework`**，
不依赖执行器：每个内建函数都是宿主函数 `fn(&mut Scope, this, &[Value]) -> Result<Value, Error>`，用
`Scope` 上的运算语义写成；需要回调 JS 时经 `Invoke`。

## 文件

| 文件 | 内建对象 |
|---|---|
| `global.rs` | 全局对象、`parseInt` / `parseFloat` / `isNaN` / `eval` / URI 函数 |
| `object.rs`、`function.rs`、`reflect.rs` | `Object`、`Function`、`Reflect` / `Proxy` |
| `array.rs`、`typed.rs`、`buffer.rs` | `Array`、TypedArray（12 种，含 Float16）、`ArrayBuffer` / `SharedArrayBuffer` / `DataView` / `Atomics` |
| `string.rs`、`text.rs` | `String`、`Symbol` |
| `regexp.rs`、`dfa.rs` | `RegExp`（[regress](https://crates.io/crates/regress) + 一个 DFA 快路） |
| `json.rs` | `JSON` |
| `math.rs`、`bigint.rs`、`date.rs` | `Math`、`BigInt`、`Date` |
| `collections.rs`、`weak.rs` | `Map` / `Set`、`WeakMap` / `WeakSet` / `WeakRef` / `FinalizationRegistry` |
| `promise.rs`、`asyncgen.rs`、`iter.rs` | `Promise`、异步生成器、迭代器与迭代器助手 |
| `error.rs` | `Error`、`TypeError` … `AggregateError`，`cause` |
| `helpers.rs` | 共用的规范步骤 |

## 覆盖范围

- **Object / Array** 的全部方法：Array 方法对类数组通用、species；`from` / `flat` / `toSorted` / `with` ……
- **反射**：`Reflect`、`Proxy`（含 revocable）；
- **迭代器助手**：全局 `Iterator`、`Iterator.from`、`map` / `filter` / `take` / `drop` / `flatMap` / `reduce` ……；
  Set 方法（`union` 等）、`Map.groupBy` / `Object.groupBy`；
- **二进制数据**：可调大小的 `ArrayBuffer`（`transfer`、detach）、可增长的 `SharedArrayBuffer`、`DataView`、
  `Atomics`（单线程语义）；
- **Date**（本地时区按 UTC 处理）；**数的格式化**（`toFixed` / `toPrecision` / `toExponential` 精确到十进制）、
  `Math.sumPrecise` / `f16round`；
- **RegExp**：Symbol 协议、`lastIndex`、`d` 标志、具名分组、species、`RegExp.escape`；字符串的
  `match` / `matchAll` / `replace` / `replaceAll` / `search` / `split`；
- **Promise** 建在 NewPromiseCapability 上：子类化、`all` / `race` / `allSettled` / `any` / `withResolvers` / `try`；
- 内建构造器按 `new.target` 定原型，所以 `class X extends Map` 之类能工作；宿主函数区分能否 `new`。

范围外：Temporal、intl402、ShadowRealm 等（见[路线图](roadmap.md)）。

## 正则：两个引擎

- 主引擎是 regress：按 UTF-16 码元匹配的回溯引擎；
- `dfa.rs` 是第二个引擎：对于 `regex` crate 在全 ASCII 输入上能与 ECMAScript 完全一致匹配的模式，用有限自动机加字面量
  预过滤代替回溯。`Dfa::new` 翻译模式，拒绝一切语义可能不同的东西：反向引用、环视、具名分组、`v` 模式、`m` 下的锚点、
  量词里的捕获组（ECMAScript 每轮重置它们）、Annex B 的怪写法、非 ASCII 的模式文本。其余情况和非 ASCII 输入仍走 regress。
- 正则字面量共享 realm 里编译好的入口与字符串；没有 `g` / `y` 的 `test()` 跳过 `lastIndex`。

## 快路

热点内建函数有快路：**可观察的行为不可能不同**时跳过完整的规范步骤——比如 `RegExp.prototype` 没被动过（pristine 检查）、
往稠密数组末尾追加、字符串与数字的拼接、JSON 解析时重复的键序列直接解析成同一个 shape。正确性以 test262 为准。

大数据的纯计算（排序的键、数的转换）可以经 `j8framework::Par` 切块放到平台线程池上并行，再把结果写回。
