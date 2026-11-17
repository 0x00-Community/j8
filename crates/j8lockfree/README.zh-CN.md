# j8lockfree

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 手写的无锁基础件（`no_std` + `alloc`），`j8framework` 与 `j8taskqueue` 共用。

| 类型 | 是什么 |
| --- | --- |
| `Fifo` | 多生产者多消费者先进先出队列（位置单调递增、分段槽表） |
| `SegVec` | 只追加的分段向量：下标稳定、可并发读；整理与取出要独占 |
| `PushList` | 只压入的 Treiber 栈：整表取走或封口（封口后压入被退回） |
| `Stock` | 库存：独占时整理成只读数组，并发时原子游标成批领取 |
| `Deque` | Chase–Lev 工作窃取双端队列：主人凭据 `Owner` 压 / 弹，别人偷 |
| `OnceBox` | 只发布一次的惰性值（并发初始化各算各的，赢的留下） |
| `AtomicRefCell` | 可跨线程的 `RefCell`：借用冲突时报错，从不等待 |

共同的做法：并发期间只增加内存（段、旧缓冲），释放推迟到独占（`&mut` 或析构）时，所以不需要危险指针、纪元回收，也没有 ABA。

## 许可

MIT，随 [j8](https://github.com/0x00-Community/j8) 一起。
