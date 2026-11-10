# r8 计划：补齐未支持的写法与已知偏差

结构改动：运算挂在 r8frame 的 `Agent` 上、经 `Invoke`（执行器实现）调回 JS；`Context` 指向 `Realm`，内建对象由 r8builtins 实现 `Intrinsics` 装进去。

范围：README「还不支持」与「已知偏差」两节里的每一项。按依赖排序分阶段，每阶段做完就在
`r8executor` 的语义测试表里加确定的用例，clippy 干净 + `no_std` 目标编过再提交。

## 阶段 1：异常与宿主函数（其余阶段的地基）

- [x] Error 对象：`Error` / `TypeError` / `ReferenceError` / `RangeError` / `SyntaxError`，带 `name` / `message`，内建错误改抛对象
- [x] `try` / `catch` / `finally`：帧内动态处理器栈（`PushHandler` / `PopHandler`），抛出时按帧展开；`finally` 走 `break` / `continue` / `return` 也要执行
- [x] 宿主函数：r8frame 定义 `NativeFn`（`ObjectKind::Native`），执行器的 `Call` 能调它
- [x] 内建对象骨架：`Object.prototype` / `Function.prototype` / `Array.prototype` / `globalThis`，挂进 `Context`
- [x] `console.log`：宿主函数，输出交给嵌入方（r8frame 定义输出 trait，r8platform 写 stdout）

## 阶段 2：对象模型

- [x] `new`：构造调用（`this` 是以 `F.prototype` 为原型的新对象，返回对象则替换）；函数带 `prototype` 属性
- [x] 类：`class` 声明 / 表达式、`constructor`、方法、静态方法、`extends` / `super(...)` / `super.m()`、字段（静态块、私有名、计算字段名还不支持）
- [x] `instanceof`、`in`、`delete`
- [x] getter / setter：访问器属性（对象字面量与类里），属性读写走访问器
- [x] ToPrimitive（提前到阶段 1 做了，Error 对象要靠它打印）：`valueOf` / `toString`，`+` / 比较 / `==` / ToString 对对象都走它

## 阶段 3：解构、展开、参数

- [x] 剩余参数 `...rest`、默认参数（含引用前面的形参）
- [x] 展开：数组字面量、调用实参（`CallSpread` / `ConstructSpread` / `SuperCallSpread`）、对象字面量（数组与字符串，迭代协议在阶段 4 接上）
- [x] 解构：声明 / 赋值 / 形参，数组与对象模式、默认值、剩余
- [x] 可选链：`a?.b` / `a?.[k]` / `a?.()`，短路到整条链末尾

## 阶段 4：迭代与生成器

- [x] 迭代协议：`Symbol`（先只做 `Symbol.iterator` 等内建 symbol）、数组 / 字符串迭代器
- [x] `for-of`（含解构绑定、`break` 时调 `return`）、`for-in`（自有 + 原型链可枚举字符串键）
- [x] 生成器：`function*` / `yield` / `yield*`；帧挂起到堆上的生成器对象，`next` / `return` / `throw` 恢复
- [x] `async` / `await`（async 生成器与 `for await` 还不支持）：Promise（`then` / `catch` / `resolve` / `reject`）、微任务队列，`async` 函数按生成器方式挂起

## 阶段 5：字面量

- [ ] 正则字面量与 `RegExp`：用 JS 语义的正则引擎（regress），`test` / `exec` / 字符串 `match` / `replace`
- [ ] BigInt 字面量与运算（num-bigint，`no_std`），`typeof` 为 `bigint`，与 Number 混算抛 TypeError

## 阶段 6：语义偏差

- [ ] 暂时性死区：`let` / `const` / `class` 声明前读写抛 ReferenceError（槽位放“未初始化”哨兵）
- [ ] 脚本顶层 `let` / `const` 放脚本作用域的声明式环境，不再是全局对象属性
- [ ] 运行中回收：执行器状态（帧、寄存器、acc）进 GC 根，按指令配额分片跑，片与片之间 `collect_debt`（`Engine` 接口改成拿 `&mut Isolate` 自己分片进入）

## 不在范围

JIT（tornado / jspass，仍只是规划）、模块（`import` / `export`）、`with` / `eval`、Proxy / Reflect、完整标准库。
