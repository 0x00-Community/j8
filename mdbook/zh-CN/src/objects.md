# 对象模型：Shape、存储与属性

## 对象布局

```text
Object  + OBJECT_SHAPE     *const Shape（null：空对象，或字典模式）
        + OBJECT_INLINE    ValueCell × INLINE_PROPS（前几个属性直接放在对象里）
        + OBJECT_STORE     存储树的 GC 槽：叶子 { 容量, ValueCell × 容量 }
```

对象把热路径要碰的东西放在可变数据（`GcCell`）外面：原型、属性（shape 加值存储）、数组元素、函数与正则的不可变数据。
JIT 按 `j8framework::Layout` 给的偏移直接读写这些字段。

j8 自己的堆结构体用 `#[j8framework::heap]` 声明，`#[mutable]` 字段按类型自动选单元（`Value` ⇒ `ValueCell`、
`ScopeLocal` ⇒ paragc `Slot`、布尔与整数 ⇒ 原子量），并生成 getter / setter：

```rust,ignore
#[j8framework::heap]
pub struct Object<'scope> {
    #[mutable(set = write_proto)]
    proto: Option<JsObject<'scope>>,      // proto() / write_proto(mc, p)
    #[mutable]
    non_extensible: bool,              // non_extensible() / set_non_extensible(mc, b)
    data: GcCell<'scope, ObjectState<'scope>>,
}
```

## Shape（隐藏类）

`shape.rs`：shape 是对象自有字符串键属性的**布局**——键与特性，按插入顺序；值另外放在对象的槽里。

- 以同样方式建出的对象共享 shape：加属性沿**转移表**走到（或新建）子 shape；
- 执行层按「shape ⇒ 槽下标」做[内联缓存](executor.md#内联缓存)；
- shape 放在 GC 堆里，通过 **Handle**（`ShapeRef`，paragc Handle 表里的一项）引用：回收器搬 shape 时改写表项，
  Handle 不变；转移表存 `Weak`，没有对象用的 shape 会被回收；
- 删属性、改特性或键太多时，对象的属性转成**字典**（`Dict`，原地修改），内联缓存不接收字典。

## 值存储

`store.rs`：属性值与数组元素放在 GC 堆里固定容量的叶子中，大了就是 128 叉树（30 万个元素三层）。
paragc 只分配定长对象，所以按容量分档：

- 叶子 `Leaf<N>`：`N` 个 `ValueCell`（`N` = 2 / 4 / … / 128），头一个字是容量；
- 内部节点 `Node`：128 个子槽，头一个字是 `NODE` 加层数。

全部 `#[repr(C)]`：不管 `N` 多大，格子都从头字之后开始，抹掉类型后也能按下标找到。读不借用、不加锁；
写经 `ValueCell::set` 走写屏障。

## 内部方法

`property.rs` 实现规范第 10.1 节（普通对象）与 10.4 节（数组、字符串包装对象）的内部方法：

- `[[GetOwnProperty]]` / `[[DefineOwnProperty]]`（ValidateAndApplyPropertyDescriptor、ArraySetLength）；
- `[[OwnPropertyKeys]]`、可扩展性、原型；
- 数组有**稠密部分**，加上按下标存的普通属性（空洞、特性各异的元素）；`length` 只读、不可扩展都按规范。

属性访问（`ops.rs`）经这些内部方法沿原型链走。Proxy 在 `proxy.rs` 里把同样的入口分派到 trap，并检查不变式。

## 字符串

`text.rs`：属性键与 JS 字符串值共用的表示。

- 不超过 22 字节的**短串**内联存放，不单独分配；
- 长串共享一份堆拷贝，从长串切出的片段（`split`、`slice`、正则匹配）指向那份拷贝而不复制（V8 的 sliced string）；
- 字符串 `+` 产生足够长的结果时先存成 **rope**（两半加总长），第一次需要内容时才拍平并缓存——`s += c` 的循环
  不再每次复制整串；拍平与释放都是迭代的，几十万层的 rope 也不会爆栈；
- 不含 `ScopeLocal`，GC 不需要管它。

当前字符串按 UTF-8 存储，所以落单的代理项（lone surrogate）会丢失；比较按 UTF-16 码元进行。

## 宿主对象

`ObjectKind::Host`：持有 Rust 值的 JS 对象（`HostData`：`GcCell` 里的 `Host`，即 `Box<dyn HostObject>`）。和 V8 的
cppgc 对象一样，宿主数据会被追踪：`HostObject::trace` 访问它持有的 JS 值；查找按 Brand 的 `TypeId`
（`HostObject::find`），所以 Rust 类型可以带 `'scope` 生命周期。
绑定方法里以 `&self` / `&mut self` 取出；重入的 `&mut self` 调用抛 `TypeError` 而不是 panic（见[绑定宏](bindings.md)）。
