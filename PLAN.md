# r8 计划：test262 核心 ECMAScript 补齐

主线是提高 test262 通过率。每个阶段：语义表加用例 → clippy 干净 → riscv `no_std` 编过 → 快照里跑 test262 基线零回归 → 相关目录 `R8_TEST262_UPDATE=1` 记新过的 → 提交推送。

不在范围：Temporal、intl402、ShadowRealm、显式资源管理（using / DisposableStack）、import defer、decorators、source phase imports。

## 阶段 1：基础设施

- [x] 动态代码：`Invoke::eval_code`，全局 `Function` / `eval`、GeneratorFunction / AsyncFunction / AsyncGeneratorFunction 构造器
- [x] 早期错误：oxc_semantic 检查、正则字面量解析时校验
- [x] 严格模式：`this` 包装、失败赋值 / 删除、未声明赋值、`arguments.callee`；严格代码里的直接 eval
- [x] 全局声明：顶层 `var` / 函数先查后建（CanDeclareGlobalVar / Function）、`undefined` / `NaN` / `Infinity` 值属性
- [x] `$262`：`global` / `evalScript` / `gc`（`createRealm` / `detachArrayBuffer` 抛 TypeError，阶段 4 接上后者）

## 阶段 2：Object / Array / 属性模型

- [x] 对象内部方法（r8frame property.rs）：[[GetOwnProperty]] / [[DefineOwnProperty]]（ValidateAndApplyPropertyDescriptor、ArraySetLength、字符串包装对象）/ [[OwnPropertyKeys]] / 可扩展性 / 原型；数组稠密部分 + 按下标存的普通属性（空洞、不同特性的元素）
- [x] `extensible`：preventExtensions / isExtensible / seal / isSealed / isFrozen
- [x] Object 补全：defineProperties、getOwnPropertySymbols / Descriptors、setPrototypeOf、fromEntries、is、hasOwn、groupBy、propertyIsEnumerable、isPrototypeOf、`__proto__` 与 `__defineGetter__` 等；字面量 `__proto__: v`
- [x] Array 方法对类数组通用，补 from / splice / copyWithin / flat / flatMap / findLast(Index) / reduceRight / toReversed / toSorted / toSpliced / with / unscopables / species
- [x] 内建函数的 `length`；宿主函数分能否 `new`，构造器经 `Agent::new_target` 按 new.target 定原型；`[Symbol.toStringTag]`

## 阶段 3：反射与迭代器

- [ ] Reflect
- [ ] Proxy（含 revocable）
- [ ] Iterator 助手
- [ ] Set 方法、Map.groupBy

## 阶段 4：二进制数据

- [ ] ArrayBuffer / SharedArrayBuffer / DataView / TypedArray（11 种）/ Atomics

## 阶段 5：Date / RegExp / 字符串数字

- [ ] Date
- [ ] RegExp 的 Symbol 协议、flags、lastIndex、`d`、compile、escape
- [ ] String 补全
- [ ] Number 格式化、Math 补全
- [ ] Symbol.for、Error 补全、URI 函数、Promise 补全与子类化、WeakRef / FinalizationRegistry

## 阶段 6：语言动态部分

- [ ] 非严格的直接 eval（外层名字表、`var` 进函数变量环境）
- [ ] `with`
- [ ] annexB 语言（块级函数提升、HTML 注释等）
- [ ] arguments 与形参联动
- [ ] 模块与动态 import
