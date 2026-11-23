# 术语表

| 术语 | 含义 |
|---|---|
| **acc（累加器）** | 字节码隐式的结果寄存器，大多数指令读写它 |
| **Agent** | 一次运行的上下文；JS 运算语义挂在它上面 |
| **Context** | 执行上下文：全局对象与内建原型 |
| **上下文槽 / Env** | 被闭包捕获的变量所在的堆上环境 |
| **CodeCache** | 多个 executor 共享的字节码缓存 |
| **deopt（去优化）** | 推测落空，从机器码回到解释器 |
| **EventLoop / EventLoopSet** | 一个 JS 事件循环 / 跑很多事件循环的 loop 线程组 |
| **反馈槽（feedback slot）** | 字节码上记录类型 / shape 信息的位置，JIT 据此推测 |
| **FrameState** | 守卫处每个解释器寄存器存着什么，去优化时据此重建帧 |
| **'scope 品牌** | 一次 `Isolate::enter` 的生命周期；带它的值出不了这次进入 |
| **GcCell / ValueCell** | j8 在 paragc 上的可变单元：整块数据的快照 / 单个值 |
| **ScopeGlobal** | 根（paragc 的），跨多次 `enter` 存活；`B` 标明类型（`ValueBrand`、`RealmBrand`） |
| **待处理异常** | 抛出的值等待的槽，照 V8；`Error::Exception` 只表示槽里有值 |
| **IC（内联缓存）** | 每个属性访问点按 shape 缓存的结果 |
| **Ignition** | V8 的解释器；j8 的字节码与它逐字节一致 |
| **Intrinsics** | 往 Realm 装内建对象的接口，j8builtins 实现 |
| **Isolate** | 独立的 JS 世界：一个 GC 堆 |
| **NaN-boxing** | 把非数的值塞进 NaN 位模式的一个 64 位字表示 |
| **OSR（栈上替换）** | 从循环中途切进机器码 |
| **paragc** | j8 用的 GC 库 |
| **Platform** | 宿主借给引擎的线程池、时钟、输出、模块加载 |
| **序言（prelude）** | 运行时装好绑定后跑的 JS，用来搭公开接口 |
| **Realm** | 规范里的领域：一套全局对象与内建 |
| **rope** | 字符串拼接的延迟表示：两半加总长 |
| **Shape（隐藏类）** | 对象属性的布局，相同建法的对象共享 |
| **短 Star** | `Star0` … `Star15`：不带操作数的单字节寄存器写 |
| **Tier** | 执行器的 JIT 钩子 |
| **TDZ（暂时性死区）** | `let` / `const` / `class` 初始化之前的区域 |
| **蹦床（trampoline）** | 每个函数一个跳转，指向它的机器码或解释器 |
| **`__j8_native`** | 运行时的隐藏注册表对象 |
