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

- [x] Reflect
- [x] Proxy（含 revocable）：r8frame proxy.rs，内部方法入口与调用 / 构造分派到 trap 并查不变式
- [x] Iterator 助手（全局 Iterator、Iterator.from、map / filter / take / drop / flatMap / reduce / toArray / forEach / some / every / find）
- [x] Set 方法、Map.groupBy

## 阶段 4：二进制数据

- [x] ArrayBuffer（可调大小、transfer、分离）/ SharedArrayBuffer（可增长）/ DataView / TypedArray（12 种，含 Float16）/ Atomics（单线程语义；多 agent 的用例跑不了）；`$262.detachArrayBuffer`

## 阶段 5：Date / RegExp / 字符串数字

- [x] Date（本地时区按 UTC）
- [x] RegExp 的 Symbol 协议、RegExpExec、flags 通用 getter、lastIndex、`d`、species、compile、escape
- [x] String 补全（fromCodePoint / raw / normalize / annexB 等；字符串还按 UTF-8 存，孤立代理项会丢）
- [x] Number 格式化（toFixed / toPrecision / toExponential 精确十进制）、Math 补全（含 sumPrecise / f16round）
- [x] Symbol.for、EvalError / URIError / AggregateError、cause、URI 函数、Promise 按 NewPromiseCapability 重写（子类、allSettled / any / withResolvers / try）、WeakRef / FinalizationRegistry（强引用）

## 阶段 6：语言动态部分

- [x] 直接 eval：调用处看得见的绑定都放进环境，CallEval 带作用域表，eval 代码接在调用处的环境链上跑；非严格的 `var` / 函数落到调用函数（已有同名用它，否则进一个按 with 查的 eval var 对象）；词法冲突报 SyntaxError；eval 里的 annexB 块级函数（形参表达式里 eval 声明 `var arguments` 的冲突规则还没做）
- [x] `with`（体内名字先查 with 对象、看 `Symbol.unscopables`，内层函数也能看到；调用的 `this` 是对象）
- [x] annexB 块级函数提升（函数与脚本，含 if 子句里的函数声明；eval 代码里的还没有）
- [x] 生成器 / async 生成器调用时就初始化形参（出错同步抛）
- [x] arguments 与形参联动（非严格、简单形参：形参放进环境，下标映射到槽位，删除 / 改访问器 / 改只读后断开）
- [x] 模块与动态 import：模块环境、导入绑定实时读出口方槽位、ResolveExport（间接 / 星号 / 歧义）、命名空间对象、import()、import.meta、`Platform::load_module`；运行器先跑 harness 脚本再跑模块（顶层 await、import defer / source phase 还没有）
