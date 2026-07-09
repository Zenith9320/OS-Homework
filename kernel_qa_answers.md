# 模拟内核理解考察题 —— 答案与基础概念解释

---

## 第一部分：模拟内核实现理解题 —— 参考答案

---

### 一、内核整体架构 (mod.rs + kernel_struct.rs)

#### Q1: 内核的模块组织结构是怎样的？

内核采用**单体式（Monolithic）架构**，在 `mod.rs` 中声明了 26 个子系统模块：

| 类别 | 模块 | 职责 |
|------|------|------|
| **核心** | `kernel_struct` | `Kernel` 结构体，系统调用分发 |
| **进程** | `task`, `process_group`, `scheduler`, `context` | 进程/线程管理、调度、上下文切换 |
| **内存** | `memory`, `address_space`, `vm` | 物理内存、虚拟地址空间、页表 |
| **文件** | `files`, `fs`, `cache`, `mount`, `io` | VFS、文件系统、缓存、挂载、磁盘 IO |
| **同步** | `locking`, `sync_queue`, `wait_queue`, `futex`, `semaphore` | 锁、等待队列、futex、信号量 |
| **IPC** | `ipc` | System V 信号量、共享内存 |
| **信号** | `signal` | POSIX 信号处理 |
| **中断** | `trap`, `timer` | 中断分发、定时器 |
| **网络** | `net` | TCP/IP 协议栈工具 |
| **安全** | `capability`, `resource` | Capability 权限、资源限制 |
| **其他** | `consts`, `elf`, `utils` | 常量定义、ELF 加载、工具函数 |

所有公开类型通过 `pub use self::*` 扁平重导出，上层只需 `use kernel::*` 即可访问全部。

#### Q2: Kernel 结构体汇集了哪些核心数据结构？

```rust
pub struct Kernel {
    pub tasks: TaskTable,              // 全局任务表（进程/线程注册中心）
    pub cache: BlockCache,             // 块缓存（N_CHAINS=64 条并发链）
    pub pool: FramePool,               // 物理页帧池（位图管理）
    pub cpus: Mutex<[Option<Arc<Task>>; MAX_CPU]>, // CPU 槽位（MAX_CPU=8）
    pub mnt: MountTable,               // 文件系统挂载表
    pub sem_store: RwLock<BTreeMap<u32, Weak<SemArr>>>, // 全局信号量存储
    pub shm_store: RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>, // 全局共享内存存储
    pub tty_buf: Mutex<VecDeque<u8>>, // TTY 终端输入缓冲区
    pub disk: Disk,                    // 磁盘设备驱动
    pub fs: SimpleFS,                  // 内存文件系统
}
```

- `cpu` 数组使用 `Mutex` 保护，因为多核可能并发修改各自槽位
- `sem_store`/`shm_store` 使用 `Weak` 引用以防止内存泄漏（当所有 `Arc` 引用释放后自动清理）
- `tty_buf` 模拟终端输入队列，最大 4096 字节

#### Q3: 全局时钟的设计是怎样的？

```rust
pub static CLK: AtomicUsize = AtomicUsize::new(0);     // 当前 CPU 时钟
pub static CLK_ALL: AtomicUsize = AtomicUsize::new(0); // 全局时钟
```

- `CLK`：模拟**每 CPU 的滴答计数器**，仅 cpu_id==0 时递增（模拟单核主时钟）
- `CLK_ALL`：**全局时钟**，每次定时器中断无论哪个 CPU 都递增
- `dtk(cpu_id)` 中：`cpu_id == 0` 时同时递增 CLK 和 CLK_ALL，否则只递增 CLK_ALL
- 这种设计模拟了真实 SMP 系统中每 CPU 都有独立 TSC(Time Stamp Counter)，但又有一个全局时间基准

#### Q4: ProcInit 的作用是什么？

`ProcInit` 负责在 `exec()` 时将命令行参数、环境变量和辅助向量（auxv）排列到用户态栈上，模拟 ELF ABI 规定的初始栈布局。

`push_at()` 的栈布局（从高地址到低地址）：
```
[栈顶]
  ...对齐填充...
  auxv 条目（键值对数组，以 AT_NULL 结束）
  环境变量指针数组（以 NULL 结束）
  命令行参数指针数组（以 NULL 结束）
  argc（参数个数）
[返回的新 sp]
```

栈需要 **16 字节对齐**（System V ABI 要求），`push_at` 末尾通过 `sp &= !0xF` 确保。

---

### 二、进程管理 (task.rs)

#### Q5: Task 结构体包含哪些资源？进程 vs 线程如何区分？

`Task` 结构体同时承担**进程**和**线程**两种角色：

| 字段 | 作用 | 进程级/线程级 |
|------|------|---------------|
| `info` | id、tag、status、fd 列表 | 进程级 |
| `parent` / `subtasks` | 父子进程关系 | 进程级 |
| `files` | 文件描述符表 | 进程级 |
| `cwd` / `exec_path` | 工作目录、可执行路径 | 进程级 |
| `pid` / `pgid` | 进程ID / 进程组ID | 进程级 |
| `threads: Vec<Tid>` | 拥有的线程ID列表 | 进程级 |
| `thd_ctx: Option<ThdCtx>` | **线程上下文**（寄存器状态） | **线程级** |
| `kstk` | 内核栈 | 线程级 |
| `addr_space` | 地址空间 | 进程级（线程共享） |
| `sig_queue` / `sig_mask` | 信号队列/掩码 | **线程级（POSIX 语义）** |
| `futexes` | Futex 桶表 | 进程级 |
| `sem_ctx` / `shm_ctx` | IPC 上下文 | 进程级 |

关键区别：一个进程（`Task`）通过 `threads` 列表"拥有"多个线程。每个线程可以有自己的 `ThdCtx`（寄存器快照），但共享同一进程的文件描述符表、地址空间、PID 等。这与 Linux 中 `task_struct` 同时作为进程和线程描述符的设计一致。

#### Q6: fork_task 的完整流程？哪些资源共享、哪些复制？

```rust
// task.rs: TaskTable::fork_task
pub fn fork_task(&self, src: &Arc<Task>) -> Arc<Task> {
    let nid = self.seq.fetch_add(1, Ordering::SeqCst); // 分配新 ID
    let tgt = Task::make(nid, &src.tag());             // 创建空 Task

    // === 复制的资源 ===
    *tgt.cwd = src.cwd 深拷贝;          // 工作目录：独立副本
    *tgt.exec_path = src.exec_path 深拷贝; // 可执行路径：独立副本
    *tgt.pgid = src.pgid;               // 进程组：继承
    *tgt.sem_ctx = src.sem_ctx.clone(); // 信号量上下文：clone（共享信号量集合引用）
    *tgt.shm_ctx = src.shm_ctx.clone(); // 共享内存上下文：clone
    *tgt.sig_mask = src.sig_mask;       // 信号掩码：复制

    // === 共享的资源 ===
    // 文件描述符表：逐项 dup（共享同一底层数据，独立的偏移量）
    for (&fd, fl) in src.files.iter() {
        tgt.files.insert(fd, fl.dup(false));
    }

    // === 新建的关系 ===
    *tgt.parent = Some(src);            // 设置父进程
    src.subtasks.push(tgt);             // 父进程记录子进程
}
```

**地址空间共享**在 `kernel_struct.rs:do_fork()` 中额外处理：
```rust
let child_as = AddrSpace::fork_from(&parent_as, child_id as u16);
// 可写区域的页帧通过 COW 共享（引用计数+1），不是立即复制
```

**copy-on-write 的核心思想**：fork 时不复制内存，而是让父子共享同一物理页，都将页表项设为只读。当任一方写入时触发 page fault，内核此时才分配新页、复制数据。

#### Q7: reap 如何处理孤儿进程？（init 收养机制）

```rust
// task.rs: TaskTable::reap
pub fn reap(&self, id: usize) {
    // 1. 将所有子进程转移给 init (PID=1)
    let ch: Vec<Arc<Task>> = t.subtasks.lock().unwrap().drain(..).collect();
    if let Some(ref r) = rt {  // rt = init 进程
        for c in ch {
            c.link_parent(r);   // 设置新父亲为 init
            r.link_child(&c);   // init 接纳新孩子
        }
    }
    // 2. 从任务表中移除
    self.map.write().unwrap().remove(&id);
}
```

**孤儿进程问题**：父进程先于子进程退出时，子进程变为"孤儿"。POSIX 规定孤儿进程必须由 init (PID=1) 收养，否则这些进程无人回收（无法 wait），且 PPID 会变为 1。这个机制确保了进程树的完整性。

#### Q8: exit_proc 执行了哪些清理操作？

```rust
pub fn exit_proc(&self, code: usize) {
    // 1. 关闭所有文件描述符
    // 2. FDT 审计：检测文件描述符表中的"空洞"
    // 3. 触发 PROC_QUIT 事件（通知本进程的等待者）
    // 4. 触发父进程的 CHILD_QUIT 事件（通知父进程，使其 wait 返回）
    // 5. 保存退出码：高8位=退出状态，低8位=终止信号
    // 6. 清空线程列表
    // 7. 设置 status = Some(code)
}
```

事件总线 `EvBus` 在此充当**异步通知机制**：子进程退出时不需要知道父进程在哪个 CPU 上运行、是否正在 wait。只需在父进程的 `ev` 上设置 `CHILD_QUIT` 标志，父进程（或 wait 代码）检查到该事件后即可做出响应。

#### Q9: has_sig 如何判断是否有未屏蔽的待处理信号？

```rust
pub fn has_sig(&self) -> bool {
    for (sig, sender) in sq.iter() {
        // 发送者过滤：只有发送给自己（sender == tid）或无特定目标（-1）的信号才算
        if snd != -1 && snd as usize != tid { continue; }
        // 信号掩码检查：signal 对应位在 sig_mask 中为 0 才未屏蔽
        if bit != 0 && (sm & bit) == 0 { found = true; break; }
    }
}
```

`sender_tid` 的过滤逻辑对应 POSIX 信号语义中的 **线程定向信号**：信号可以发送给特定线程（`sender_tid != -1`），只有目标线程才能递送该信号。

#### Q10: clone_thread vs fork_task 的区别？

| 方面 | `fork_task` | `clone_thread` |
|------|-------------|----------------|
| **用途** | 创建子进程 | 创建线程 |
| **文件描述符表** | 逐项 dup（独立偏移量） | 未复制（线程共享进程的 fd 表） |
| **地址空间** | COW 共享 | 共享同一地址空间 |
| **PID** | 分配新 PID | 分配新 TID，归入原进程的 `threads` 列表 |
| **栈** | 保持父进程栈位置 | 使用 `stack_top` 参数指定独立的用户态栈 |
| **线程上下文** | 复制父进程的 thd_ctx | 新建 ThdCtx，设置 TLS、clear_tid |
| **返回值** | 父进程 `begin_run` 返回原 ctx | 子线程 `set_ret(0)` 返回 0 |

---

### 三、调度器 (scheduler.rs)

#### Q11: RunQueue::enqueue 的排序依据是什么？

排序使用**综合得分 = 优先级因子 + vruntime - 权重**：

```rust
let score_a = prio_a * 1000 - nice_a * 50 + vruntime_a - weight_a;
let score_b = prio_b * 1000 - nice_b * 50 + vruntime_b - weight_b;
```

**分数越低越优先**（排序时 score_a > score_b 则交换）。

各因素含义：
- **prio × 1000**：动态优先级，值越小优先级越高（Linux 中 -20 最高，19 最低）
- **nice × 50**：nice 值的惩罚项，nice 越高（越"友好"）得分越高越不易被选中
- **vruntime**：CFS 虚拟运行时间，运行时间越长 vruntime 越大，越容易被换下
- **weight**：CFS 权重，高权重（低 nice）的任务获得更多 CPU 份额

#### Q12: CFS 权重计算的阶梯函数

```rust
pub fn weight(&self) -> u64 {
    match self.nice {
        n if n < -10 => 88761,  // 极高优先级
        n if n < 0   => 29154,  // 高优先级
        0            => 1024,   // 默认
        n if n < 10  => 335,    // 低优先级
        _            => 110,    // 最低优先级
    }
}
```

这与 Linux CFS 的权重表一致（简化为 5 档）。**nice 值相差 1 意味着 CPU 份额差距约 10%**：
- nice 0 权重 = 1024
- nice 1 权重 ≈ 1024 / 1.25 ≈ 820

**调度理念**：CFS 不是按固定时间片分配，而是按**权重比例**分配 CPU 时间。vruntime 的增长速度与权重成反比——高权重任务的 vruntime 增长慢，因此更容易被调度。

#### Q13: compute_load_balance 的负载均衡评分模型

```rust
score = -(tc) * 100       // 任务数越少越好（负惩罚）
      + pr * 10           // 优先级越高越好
      - (blocked ? 500 : 0) // IO 阻塞的 CPU 大幅降分（避免选它）
      + cache_bonus       // 缓存亲和性加分（TC>0 时+50）
      + numa_factor        // NUMA 邻近加分（近端 +10，远端 -10）
```

- **缓存亲和性（Cache Affinity）**：如果某个 CPU 上已有任务在运行（`tc > 0`），给它 +50 分，倾向于将新任务分配到"热"CPU（减少缓存miss）
- **NUMA 因子**：将 CPU 分为前一半（"近端"）和后一半（"远端"），近端 CPU +10，远端 -10。模拟 NUMA 架构中访问本地内存比远端内存快的特性

#### Q14: preempt_disable / preempt_enable 的抢占控制

```rust
pub fn preempt_disable(&self) {
    self.preempt_count.fetch_add(1, Ordering::Relaxed);
}
pub fn preempt_enable(&self) {
    let prev = self.preempt_count.fetch_sub(1, Ordering::Relaxed);
    if prev == 1 {
        // count 降为 0：检查是否需要重新调度
        let _need_resched = self.queue.len() > 0;
    }
}
```

`preempt_count` 是一个**嵌套计数器**（非布尔值），而非简单的开/关标志。这允许内核代码嵌套地禁用抢占：

```c
preempt_disable();  // count = 1
  preempt_disable();  // count = 2
  preempt_enable();   // count = 1（不触发调度检查）
preempt_enable();   // count = 0（触发调度检查！）
```

**为什么降为 0 时需要检查重新调度？**因为在抢占被禁用的时间段内，可能有更高优先级的任务被唤醒或时间片到期。`preempt_enable` 是一个"调度点"——此时检查是否有更紧急的任务需要运行。

#### Q15: rebalance 中 vruntime 更新公式

```rust
let delta = (tick * 1024) / w;
policy.vruntime = policy.vruntime.wrapping_add(delta);
```

**公式含义**：
- `tick`：实际经过的时间片
- `1024`：nice=0 对应的基准权重（归一化常数）
- `w`：当前任务的权重

**原理**：vruntime 的增长速度与实际时间的比值 = `1024 / weight`。高权重任务（nice < 0）的 vruntime 增长慢，因此"看起来"运行时间少，更容易被 CFS 选中；低权重任务（nice > 0）的 vruntime 增长快，更容易被换下。这就实现了比例公平调度。

---

### 四、内存管理

#### Q16: FramePool 如何管理物理页帧？位图分配的时间复杂度？

`FramePool` 使用 `Vec<bool>` 作为位图，每个 bool 对应一个物理页帧：
- `true` = 空闲
- `false` = 已分配

**分配算法**：线性扫描，O(n)：
```rust
for (i, f) in s.iter_mut().enumerate() {
    if *f { *f = false; return Some(i); }
}
```

**连续分配** (`get_contig`)：按对齐步长跳跃扫描，查找连续的 `sz` 个空闲帧：
```rust
for start in (0..s.len()).step_by(a) {
    if (start..start + sz).all(|i| s[i]) { /* 分配 */ }
}
```

#### Q17: Buddy 分配器的伙伴地址计算公式

```rust
// 释放时寻找伙伴并向上合并
let buddy_addr = current_addr ^ block_size;
// block_size = (1 << current_order) * PAGE_SZ
```

**原理**：Buddy 系统中，两个伙伴块是相邻的，且它们的起始地址仅在"当前 order 对应的位"上不同。XOR 运算恰好翻转该位：

```
例如 order=2, block_size=16KB (4页)
块 A: 0x0000 (二进制 ...0000 0000)
块 B: 0x4000 (二进制 ...0100 0000)  ← XOR 16KB 翻转第14位
A ^ 0x4000 = B
B ^ 0x4000 = A
```

**alloc_order 的拆分流程**：
1. 从请求的 order 开始，向上查找有空闲块的 order
2. 每下降一个 order，将当前块一分为二，一半分配，一半放入空闲链表
3. 直到达到请求的 order

#### Q18: SharedPage 的 COW 状态机

三个原子变量的状态转换：

```
初始状态: frame=orig_frame, pending=true, w=false
         |
         | fork (引用计数+1, 页表项设为只读)
         v
共享状态: frame=orig_frame, pending=true, w=false  (rc >= 2)
         |
         | 写操作触发 fault()
         v
COW 解决: frame=new_frame, pending=false, w=true   (rc = 1, 独享)
         |
         | 再次 fault (已解决)
         v
直接返回: Ok(cur)  // pending=false，直接返回当前帧
```

`fault()` 方法的关键逻辑：
```rust
if !pending { return Ok(cur); }  // 已解决，直接返回
// 分配新帧 → 更新 frame → pending=false, w=true
```

#### Q19: AddrSpace::fork_from 中对 VM_WRITE 区域的两次 ref_up

```rust
// 第一次：在复制 VmRegion 时
if region.flags & VM_WRITE != 0 {
    region.ref_up();  // 为子进程增加引用
}
// 第二次：在复制 cow_pages 时
for region in parent.vm_map.regions.iter() {
    if region.flags & VM_WRITE != 0 {
        region.ref_up();  // 又增加一次引用
    }
}
```

实际上这里**有一次是冗余的**（代码中的 bug 或是为了双重追踪）。正确的 COW fork 语义应该是：

1. 子进程的 VmRegion 初始 ref_count = 1（自己持有引用）
2. 可写区域的 VmRegion 被父子共享，父进程持有一个引用，子进程也持有一个引用
3. 可写区域的**页帧**（PgFrame）也需要增加引用计数，因为父子共享同一个物理页

正确的 `ref_up` 次数 = 区域共享数 + 页帧共享数，而代码中出现两次 `ref_up` 可能意味着该同学意图分别增加 VmRegion 级别和 PgFrame 级别的引用计数。

#### Q20: handle_cow_fault 的三种情况

```rust
pub fn handle_cow_fault(&self, addr: usize, pool: &FramePool) -> Result<usize, &'static str> {
    // 情况1：页已映射 + rc == 1（独享）→ 原地写，不分配新帧
    if rc <= 1 { return Ok(old_frame_id * PAGE_SZ + MEM_OFF); }

    // 情况2：页已映射 + rc >= 2（父子共享）→ COW 断联
    //   - 分配新物理帧
    //   - 旧帧引用计数减1（本进程不再共享）
    //   - 新帧独享（rc=1）
    //   - 替换 cow_pages 中的映射

    // 情况3：页从未映射 → 首次分配
    //   - 直接从 FramePool 拿一个新帧
    //   - 加入 cow_pages
}
```

**物理地址计算公式**：`frame_id * PAGE_SZ + MEM_OFF`

#### Q21: VmMap::find 使用二分查找的前提

```rust
pub fn find(&self, addr: usize) -> Option<&VmRegion> {
    let mut lo = 0; let mut hi = n;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if addr < r.base { hi = mid; }         // 目标在左侧
        else if addr >= r.base + r.len { lo = mid + 1; } // 目标在右侧
        else { return Some(r); }               // 命中
    }
}
```

**前提**：`regions` 列表按 `base` 地址**升序排列**。`insert()` 方法在插入时保证有序性：从头遍历找到第一个 `base > new_region.base` 的位置，插入前面。同时检测重叠。

#### Q22: ZoneInfo 的水位线机制

- **low_watermark**：低水位线。当空闲页数低于此值时触发 kswapd 内存回收
- **high_watermark**：高水位线。回收内存的目标值
- `zone_can_alloc()`：空闲页 > low_watermark 时可以继续分配
- `zone_pressure()`：计算 0-100 的内存压力值（低于 low=100，高于 high=0，中间线性）
- `reclaim_target()`：需要回收 `high_watermark - free` 个页面

这与 Linux 内核的 `zone_watermark` 机制一致，用于**异步内存回收**（kswapd）和**直接回收**（direct reclaim）的决策。

#### Q23: Slab 分配器的空闲链表组织

```rust
pub struct SlabEntry {
    pub data: Vec<u8>,           // 原始字节数组
    pub obj_size: usize,         // 对齐后的对象大小
    pub capacity: usize,         // 最大对象数
    pub free_list: VecDeque<usize>, // 空闲偏移量链表（FIFO）
}
```

- **分配** O(1)：`free_list.pop_front()` 取第一个空闲偏移量
- **释放** O(1)：将偏移量 `push_back` 回空闲链表
- **对齐**：`obj_size = (raw_size + SLAB_ALIGN - 1) & !(SLAB_ALIGN - 1)`（8字节对齐）

Slab 分配器的核心优势：**减少内存碎片**（同大小对象集中管理）和**缓存热度**（对象复用保留了缓存中的旧数据）。

#### Q24: p2v / v2p 地址转换

内核使用**直接映射（Direct Mapping）**方案：
- `PHYS_OFF = 0xFFFF_FFFF_0000_0000`（物理地址在虚拟地址空间中的基址）
- `p2v(pa) = PHYS_OFF + pa`（物理地址加上固定偏移得到虚拟地址）
- `v2p(va) = va - PHYS_OFF`（减去偏移得到物理地址）

**直接映射的好处**：
1. 转换速度极快（不需要查页表）
2. 内核可以像访问普通内存一样访问任何物理地址
3. 简化了 DMA 和内存映射 IO 的实现

但这也意味着内核空间与物理内存是一一对应的，限制了内核的虚拟地址灵活性。

---

### 五、文件系统 (files.rs + fs.rs)

#### Q25: FLike 联合枚举的设计意图

```rust
pub enum FLike {
    File(FHandle),   // 普通文件
    Pipe(PipeNode),  // 管道
    Ep(EpInst),      // epoll 实例
}
```

这是**多态（Polymorphism）的 enum 实现**，等价于 VFS 中的 `file_operations` 虚函数表：

- **统一接口**：`read/write/ioctl/poll/dup` 等方法对所有类型统一调用
- **文件描述符表简化**：`BTreeMap<usize, FLike>` 可以混合存储不同类型的文件对象
- **扩展性**：添加新文件类型只需增加 enum 变体和对应方法分支

这正是 Linux "一切皆文件" 哲学在这款模拟内核中的体现。

#### Q26: PipeNode 的管道关闭检测

```rust
pub fn can_read(&self) -> bool {
    let d = self.data.lock().unwrap();
    d.buf.len() > 0 || d.ends < 2  // 有数据 OR 写端已关闭
}
```

`ends` 初始为 2（读端+写端）。当任一端的 `PipeNode` 被 Drop 时：
```rust
impl Drop for PipeNode {
    fn drop(&mut self) {
        d.ends -= 1;
        d.bus.set(EvFlag::CLOSED);  // 通知对端
    }
}
```

**读端在缓冲区为空且写端存活时返回 `Err("again")`**（对应 Linux 的 EAGAIN），告诉调用者应该阻塞等待（如果是阻塞模式）或稍后重试（如果是非阻塞模式）。

#### Q27: EpInst 三个字段的作用

- `events: BTreeMap<usize, EpEvent>`：被监控的 fd → 事件配置（关注哪些事件及触发模式）
- `ready: Arc<Mutex<BTreeSet<usize>>>`：已就绪的 fd 集合（由事件通知机制填充）
- `new_ctl: Arc<Mutex<BTreeSet<usize>>>`：新添加/修改的 fd（待处理的 EPOLL_CTL_ADD/MOD）

**EPOLLET（边缘触发）** 在这个简化实现中未实际实现与 LT 的区别——都需要通过 `ready` 集合来判断。

真正的区别：
- **LT（Level-Triggered，电平触发）**：只要 fd 可读/可写，每次 `epoll_wait` 都会返回该 fd
- **ET（Edge-Triggered，边缘触发）**：仅在状态从"不可读"变为"可读"时返回一次，之后即使仍可读也不再通知（要求用户一次性读完所有数据）

#### Q28: Channel::recv 的完整阻塞接收流程

```
1. 自旋获取 guard 锁
2. 尝试从环形缓冲区读取一个字节
3. 如果成功 → 释放 guard，返回
4. 如果失败（缓冲区空）
   a. 检查关闭标志 → 如果已关闭，释放 guard 返回 None
   b. 将当前线程加入等待队列
   c. 释放 guard
   d. thread::park()  ← 阻塞等待
   e. 被唤醒后重新自旋获取 guard
   f. 再次尝试读取
5. 释放 guard，返回结果
```

**关键设计**：在 park 前释放 guard 锁，避免死锁（如果持有锁期间 park，唤醒者无法获取锁来生产数据）。

---

### 六、同步原语

#### Q30: KernLock 的可重入机制

```rust
pub fn enter(&self, id: usize) {
    let cur_tid = Self::current_tid_u64();
    let owner = self.owner_tid.load(Ordering::Relaxed);

    // 同线程可重入
    if owner == cur_tid && id != 0 {
        self.holder_stack[d].store(id, Ordering::Relaxed); // 入栈
        self.depth.fetch_add(1, Ordering::Relaxed);        // 深度+1
        return;  // 不需要自旋！
    }

    // 不同线程：自旋获取锁
    while self.flag.compare_exchange(false, true, ...).is_err() {
        core::hint::spin_loop();
    }
    // 获取成功：设置 owner_tid、depth=1
}
```

**为什么同线程不需要再自旋？**因为同一线程不会被自己抢占——如果当前线程正在执行临界区代码，它不可能同时在另一个调用栈帧中等待同一个锁。重入只是增加嵌套计数。

**holder_stack 的作用**：
- 追踪每层嵌套的调用者标识（id），用于 `leave()` 时的配对校验
- 深度不能超过 16（`HOLDER_STACK_CAP`）
- `leave(id)` 检查 `holder_stack[depth-1] == id`，不匹配说明 enter/leave 调用配对错误

#### Q33: FutexBucket 中 per-waiter AtomicBool 的作用

**虚假唤醒问题**：`thread::park()` 可能被虚假唤醒（操作系统层面的伪通知），导致等待者醒来时 futex 值并未真正改变。

**解决方案**：
```rust
// 等待侧
let flag = Arc::new(AtomicBool::new(false));
// ...入队...
thread::park();
if flag.load(Ordering::Relaxed) { Ok(()) }  // 真实唤醒
else { Err("timeout") }                      // 虚假唤醒/超时

// 唤醒侧
f.store(true, Ordering::Relaxed);  // 先设置标志
t.unpark();                        // 再唤醒
```

这种"先设标志再唤醒"的顺序确保了：即使线程在 `flag.store(true)` 和 `t.unpark()` 之间被调度走，被唤醒者也能正确判断这是真实唤醒。这个设计模式在 Linux futex 的 `WAKE` 操作中也有对应。

#### Q35: SemaGuard 的 RAII 设计

```rust
pub fn access(&self) -> Result<SemaGuard<'_>, &'static str> {
    self.acquire_spin()?;  // P 操作
    Ok(SemaGuard { s: self })
}
impl Drop for SemaGuard<'_> {
    fn drop(&mut self) { self.s.release(); }  // V 操作（自动！）
}
```

**解决的问题**：
1. **异常安全**：即使临界区代码 panic，guard 析构时也会自动 release，不会造成死锁
2. **防止忘记 release**：程序员不需要手动配对 P/V 操作
3. **资源管理自动化**：符合 C++ RAII/Rust ownership 的最佳实践

---

### 七、IPC (ipc.rs)

#### Q36: SemArr::get_or_create 如何处理 IPC 标志

```rust
if k == 0 {  // IPC_PRIVATE：分配新 key
    k = (1u32..).find(|i| m.get(i).is_none()).unwrap();
} else if let Some(w) = m.get(&k) {
    if let Some(a) = w.upgrade() {
        // IPC_CREAT | IPC_EXCL：如果已存在则报错
        if (flags & IPC_CREAT) != 0 && (flags & IPC_EXCL) != 0 {
            return Err("eexist");
        }
        return Ok(a);  // 返回已有集合
    }
}
```

`Weak` 引用的作用：全局存储使用 `Weak<SemArr>` 而非 `Arc<SemArr>`，这样当所有进程释放对信号量集合的引用后，信号量集合可以被自动释放，不会在全局存储中形成内存泄漏。

#### Q37: SemCtx 的 undos 撤销机制

```rust
impl Drop for SemCtx {
    fn drop(&mut self) {
        for (&(id, num), &op) in &self.undos {
            match op {
                1 => arr[num as usize].release(),  // 撤销 P 操作（+1）
                _ => {}
            }
        }
    }
}
```

**为什么需要撤销**：进程在持有信号量期间异常退出（如被 SIGKILL 杀死），如果不撤销之前的 P 操作，信号量计数会永久性减少，导致其他进程永久等待（死锁）。System V 信号量的 `SEM_UNDO` 标志就是为解决这个问题而设计的。

#### Q38: 共享内存中使用 Weak 解决的生命周期问题

`shm_store: RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>`

- `Arc<Mutex<Vec<usize>>>` 被所有附加（attach）该共享内存的进程持有
- 全局 store 只持有 `Weak` 引用，不影响引用计数
- 当最后一个进程 detach（Drop 其 Arc），共享内存自动释放
- 新进程通过相同的 key 再次 attach 时，`upgrade()` 失败（返回 None），触发重新分配

这避免了手动引用计数管理（shmctl IPC_RMID 的复杂语义）。

---

### 八、信号处理

#### Q39: 信号的三态模型

```
生成 (Generation) → 挂起 (Pending) → 递送 (Delivery)
                      ↑                    ↓
                   阻塞 (Blocked)      执行处理函数
```

- `sig_queue: VecDeque<(i32, isize)>`：待处理信号队列（带发送者信息）
- `sig_mask: u64`：阻塞掩码（对应位=1 表示阻塞该信号）
- `pending: u64`：挂起位图（对应位=1 表示有信号待处理）

`coalesce_pending()` 返回 `pending & !blocked`：挂起且未被阻塞的信号集合。

#### Q40: SIGKILL 和 SIGSTOP 的特殊性

内核中多处对这两个信号做了特殊处理：

```rust
// signal.rs: sig_block/sig_setmask 中
self.blocked &= !((1u64 << SIGKILL) | (1u64 << SIGSTOP));

// kernel_struct.rs: sigprocmask 中
let unmaskable: u64 = (1u64 << SIGKILL) | (1u64 << SIGSTOP);
*mask = (*mask | new_set) & !unmaskable;

// sigaction 中
if signo == SIGKILL || signo == SIGSTOP { return Err("einval"); }
```

**原因**：SIGKILL 必须能终止任何进程（否则无法杀死恶意/死循环进程），SIGSTOP 必须能暂停任何进程（这是 job control 的基础）。如果它们可以被阻塞或自定义处理函数，系统将失去对进程的最终控制权。

#### Q41: exec 后清除信号处理函数

```rust
pub fn clear_non_caught(&mut self) {
    for action in self.actions {
        if action.handler != SIG_DFL && action.handler != SIG_IGN {
            action.handler = SIG_DFL;  // 重置为默认
        }
    }
}
```

**保留 SIG_IGN（忽略）的原因**：如果用户在 exec 前显式设置了忽略某个信号（如 SIGINT），说明用户明确不想要默认行为。如果 exec 后恢复了默认处理，用户进程可能被意外中断。这是 POSIX 规定的语义。

---

### 九、系统调用分发

#### Q43: SYS_FORK 中内存压力检查

```rust
let _mem_pressure = {
    let ratio = (used * 100) / N_FRAMES;
    if ratio > 90 { return Err("enomem"); }  // 拒绝 fork
};
let _child_copy_cost = { /* 估算子进程需要的内存 */ };
if avail_after < _child_copy_cost / PAGE_SZ {
    return Err("enomem");
}
```

**为什么 fork 时只返回新 PID 而不实际复制？**

因为这是一个**简化的模拟实现**。真正的 fork 需要在 `do_fork()` 中调用 `AddrSpace::fork_from()` 来设置 COW 地址空间。`dispatch_syscall` 中的 `SYS_FORK` 只做可行性检查（内存足够？），实际的进程创建在 `do_fork()` 中完成。这种"先检查后执行"的设计模拟了 Linux 内核中 `copy_process` 的流程。

#### Q44: SYS_EXEC 中硬编码的 ELF 头

```rust
let elf_data = vec![
    0x7f, b'E', b'L', b'F',  // ELF Magic
    2,    // EI_CLASS = 64-bit
    1,    // EI_DATA = little-endian
    1,    // EI_VERSION
    0,    // EI_OSABI = System V
    // ...
    2, 0, // e_type = ET_EXEC (可执行文件)
    0x3e, 0, // e_machine = EM_X86_64 (x86_64)
    // ...
];
```

这个字节数组代表一个典型的 **x86_64 Linux ELF 可执行文件**的头部。模拟内核中用它做占位验证，实际完整实现中这些数据应该从磁盘上的可执行文件读取。

#### Q45: SYS_WAIT4 中 pid 参数的四种语义

| pid 值 | 语义 | 代码逻辑 |
|--------|------|---------|
| **-1** | 等待任意子进程 | 遍历 `zombie_tasks()`，取第一个 |
| **0** | 等待同一进程组的子进程 | 按 pgid 筛选子进程 |
| **>0** | 等待指定 PID 的子进程 | 精确匹配 `t.id() == target` |
| **<-1** | 等待进程组 ID = `-pid` 的任意子进程 | 用 `pgid_group(-pid)` 查找 |

**WNOHANG 选项**：如果 `WNOHANG` 被设置且没有符合条件的已退出子进程，`wait4` 立即返回 0（而非阻塞等待）。

#### Q46: copy_to_user / copy_from_user 为什么不能直接用 memcpy？

```rust
pub fn copy_to_user(&self, start: usize, data: &[u8]) -> Result<(), &'static str> {
    // 逐页处理：
    // 1. 从 cow_pages 获取该虚拟页对应的物理帧
    // 2. 通过 pool.write_frame() 写入帧数据
}
```

**原因**：
1. **用户态地址可能无效**：野指针、未映射地址、越界地址——直接 memcpy 会导致内核 panic
2. **COW 语义**：写入用户页可能需要触发 COW 断联，分配新物理帧
3. **权限检查**：需要确保内核有权访问该用户地址范围
4. **SMAP/SMEP**：真实硬件上，内核不能直接访问用户态内存（需要临时关闭 SMAP 或使用专用指令）

`ensure_user_range` 逐页调用 `handle_pgfault` 的目的：提前触发可能的缺页异常，确保整段地址范围都有有效的物理页映射。这模拟了 Linux 的 `fixup_user_fault` 机制。

#### Q48: SYS_EPOLL_WAIT 中超时精度

```rust
let ticks_to_wait = (timeout as usize) * TIMER_TICK_HZ / 1000;
// TIMER_TICK_HZ = 100 (每秒100个tick，即10ms一个tick)
// timeout 单位是毫秒
```

**精度分析**：
- 最小等待时间：10ms（一个 tick）
- 1ms 超时 → 0 tick（立即返回，timeout=0 的表现）
- 15ms 超时 → 1 tick（实际等待 ~10ms）
- 精度受 tick 频率限制，误差 ±10ms

真实 Linux 使用高精度定时器（hrtimer），精度可达纳秒级。

---

## 第二部分：操作系统基础概念解释

---

### 一、进程与线程

#### 1. 进程控制块 (PCB) 应包含哪些信息？

PCB 是操作系统用来描述和管理进程的数据结构。结合本内核的 `Task` 结构体：

| 类别 | 包含信息 | 对应字段 |
|------|---------|---------|
| **进程标识** | PID、PPID、PGID | `pid`, `parent`, `pgid` |
| **处理器状态** | 通用寄存器、IP、SP、FLAGS | `thd_ctx → Context` |
| **进程调度** | 优先级、时间片、调度策略 | `SchedulePolicy` |
| **内存管理** | 地址空间、页表基址、brk | `addr_space`, `vm_token` |
| **文件管理** | 文件描述符表、工作目录 | `files`, `cwd` |
| **IPC** | 信号量上下文、共享内存 | `sem_ctx`, `shm_ctx` |
| **信号** | 待处理信号、信号掩码 | `sig_queue`, `sig_mask` |
| **资源限制** | 打开文件数、栈大小等 | `ResourceLimits` |

#### 2. fork() 的写时复制 (COW) 原理

**传统 fork 的问题**：子进程全量复制父进程的地址空间，耗时且浪费内存（因为子进程通常立即 exec，之前复制的内存全部作废）。

**COW 优化**：
1. fork 时，父子进程共享所有物理页，将这些页的页表项标记为**只读**
2. 同时为每个共享页增加引用计数
3. 当任一方尝试写入时，CPU 触发**写保护缺页异常**
4. 缺页处理程序检查引用计数：
   - rc == 1：只有自己在用，直接改为可写即可
   - rc >= 2：分配新物理页，复制数据，更新页表，旧页引用计数-1

**效率分析**：避免了不必要的内存复制，只有当真的需要写入时才复制。实测中 COW fork 比传统 fork 快 10-100 倍。

#### 3. 孤儿进程和僵尸进程

**僵尸进程 (Zombie)**：
- 子进程已退出，但父进程尚未调用 `wait()` 获取其退出状态
- PCB 仍被保留（包含退出码），但所有其他资源已释放
- 过多的僵尸进程会消耗 PID 资源（因为 PCB 占用内存）

**孤儿进程 (Orphan)**：
- 父进程先于子进程退出
- POSIX 规定由 init (PID=1) 收养孤儿进程
- init 定期调用 `wait()` 回收这些进程
- 孤儿进程不会变成僵尸（因为 init 会 wait 它们）

本内核中：`exit_proc` 将子进程转移给 init → `reap` 也执行同样的转移逻辑。

#### 4. 进程组和会话

- **进程组 (Process Group)**：一组相关进程，用于 job control。前台进程组可以访问终端，后台进程组访问终端时收到 SIGTTOU/SIGTTIN
- **会话 (Session)**：一组进程组的集合。每个会话有一个**控制终端**（controlling terminal）
- **setsid 的限制**：进程组 leader 不能调用 setsid。因为 leader 的 PGID == PID，如果允许它创建新会话，它的 PGID 会被重置，同一进程组中的其他成员将失去 leader

#### 5. clone() 与 fork() 的区别

| 资源 | fork() | clone(CLONE_VM) | clone(CLONE_FILES) |
|------|--------|-----------------|---------------------|
| 地址空间 | COW 共享 | 完全共享 | COW 共享 |
| 文件描述符表 | 复制 (dup) | 复制 | 共享 |
| 信号处理 | 复制 | 共享 | 复制 |
| PID | 新 PID | 新 PID（同线程组） | 新 PID |
| 栈 | 复制 | 新栈 | 复制 |

Linux 的 `clone()` 是 fork 的超集，通过不同标志位组合可以实现 fork、vfork、pthread_create 等多种语义。

---

### 二、CPU 调度

#### 6. CFS 的核心思想

CFS (Completely Fair Scheduler) 的目标是**在所有可运行进程间公平分配 CPU 时间**。

**核心机制**：
- 每个进程有一个 `vruntime`（虚拟运行时间），记录其"应得"的 CPU 时间
- vruntime 的增长与实际时间成正比，与权重成反比：`vruntime += real_time * NICE_0_LOAD / weight`
- 调度器总是选择 vruntime **最小**的进程运行（红黑树查找 O(log n)）
- 权重由 nice 值决定：nice 越低，权重越大，vruntime 增长越慢

**公平性的含义**：不是每个进程运行相同时间，而是按比例分配。nice 0 的进程比 nice 5 的进程多获得约 50% 的 CPU 时间。

#### 7. 抢占的条件

**用户态抢占**：
- 系统调用返回或中断返回时，检查 `need_resched` 标志
- 如果标志被设置，触发调度

**内核态抢占**：
- 仅在 `preempt_count == 0` 时允许
- 持有自旋锁、在中断上下文中、禁用抢占时不可抢占
- 本内核中通过 `RunQueue::preemptible()` 检查

**抢占的必要性**：提高系统响应性。没有内核抢占，一个在内核中执行长时间操作的低优先级任务会延迟高优先级任务的运行。

#### 8. 负载均衡

多核系统中的 CPU 调度器需要定期检查各 CPU 间的负载是否均衡：

- **push migration**：过载 CPU 将任务推送到空闲 CPU
- **pull migration**：空闲 CPU 从过载 CPU 拉取任务
- **NUMA 意识**：优先将任务迁移到同一 NUMA 节点的 CPU
- **缓存亲和性**：尽量避免跨 CPU 迁移（保留热缓存）

本内核中 `compute_load_balance` 实现了简化版的评分模型。

#### 9. I/O 密集型 vs CPU 密集型任务的调度

- **I/O 密集型**：频繁阻塞等待 I/O，运行时间短，交互性强
- **CPU 密集型**：长时间占用 CPU，批处理性质

CFS 对两类任务的处理：
- I/O 密集型任务阻塞期间 vruntime 不增长 → 被唤醒后 vruntime 很小 → 立即被调度（获得良好的交互响应）
- CPU 密集型任务持续运行 → vruntime 快速增长 → 很快被换下

**优先级提升**：本内核中 `boost_priority` 短暂提高任务的动态优先级，用于解决优先级反转问题或改善交互任务的响应延迟。

---

### 三、内存管理

#### 10. 虚拟地址 vs 物理地址

- **物理地址**：内存芯片上的真实地址，由内存控制器直接使用
- **虚拟地址**：CPU 发出的地址，需要通过 MMU（内存管理单元）转换为物理地址

**MMU 转换流程**：
```
虚拟地址 = [页目录索引 | 页表索引 | 页内偏移]
    ↓
CR3 → 页目录 → 页表 → 物理页基址 + 页内偏移 → 物理地址
```

**TLB (Translation Lookaside Buffer)**：硬件缓存最近的虚拟→物理转换结果，避免每次都查页表。

**直接映射区**：内核将全部（或大部分）物理内存映射到内核地址空间的一个连续区间（本内核中 `PHYS_OFF = 0xFFFF_FFFF_0000_0000`）。好处是 `v2p/p2v` 只需加减一个偏移量。

#### 11. 分页与分段

| 特性 | 分段 | 分页 |
|------|------|------|
| 划分单位 | 可变大小（如代码段、数据段） | 固定大小（通常 4KB） |
| 程序员可见性 | 可见（段寄存器） | 透明 |
| 外部碎片 | 有 | 无 |
| 内部碎片 | 无 | 有（最后一页） |
| 共享/保护 | 段级别 | 页级别 |

现代操作系统以分页为主（x86_64 长模式下分段基本被废弃）。4KB 页面是工程权衡：太小→页表庞大、TLB miss 多；太大→内部碎片严重。

#### 12. 伙伴系统 (Buddy System)

**核心思想**：以 2 的幂次方为大小分配内存块。

**分配**：从请求的 order 开始向上搜索，找到空闲块后向下拆分
```
请求 order=1 (2页 = 8KB)
  order=3 (32KB) 空闲块: ████████
  拆分为两个 order=2: ████ ████ (一个分配，一个放回)
  再拆分 order=2→order=1: ██ ██ ████ (分配第一个 order=1)
```

**释放**：释放后尝试与"伙伴"合并
```
伙伴地址 = 块地址 ^ (1 << order) * PAGE_SIZE
例如：地址 0x0000 的 order=1 块的伙伴是 0x2000
如果 0x2000 的 order=1 块也空闲 → 合并为 0x0000 的 order=2 块
```

**碎片问题**：Buddy 系统解决了外部碎片（所有块都是 2^n 大小，不存在无法满足的对齐问题），但内部碎片仍然存在（分配 5KB 需要 8KB 的块）。

#### 13. Slab 分配器

**解决问题**：内核频繁分配/释放固定大小的对象（如 inode、dentry、task_struct），每次用 Buddy 系统分配太慢且浪费（大量内部碎片）。

**工作原理**：
1. 从 Buddy 系统申请大块内存（1-N 页）
2. 将大块切分为固定大小的对象槽位
3. 用空闲链表管理对象的分配和释放
4. 分配/释放都是 O(1) 操作

**着色（Coloring）**：通过调整起始偏移，避免不同 slab 中相同偏移的对象映射到同一缓存行（减少缓存冲突）。

#### 14. 内存区域 (Zone)

真实内核中物理内存分为不同区域：

- **ZONE_DMA**：0-16MB，用于 ISA 设备的 DMA（只能访问低 16MB）
- **ZONE_DMA32**：0-4GB，用于 32 位设备的 DMA
- **ZONE_NORMAL**：内核直接映射区域
- **ZONE_HIGHMEM**（32位系统）：超出内核直接映射范围的内存，需要临时映射

**水位线机制**：
- **high**：系统健康运行的空闲页水平
- **low**：低于此值触发 kswapd（内核交换守护进程）开始回收
- **min**：低于此值触发直接回收（分配者自己回收），分配可能阻塞

```
空闲页: [████████████████░░░░░░░░]  ← high
        [████████░░░░░░░░░░░░░░░░]  ← low → kswapd 启动
        [██░░░░░░░░░░░░░░░░░░░░░░]  ← min → 直接回收
```

#### 15. 缺页异常 (Page Fault)

**触发场景**：
1. 访问未映射的虚拟地址 → SIGSEGV
2. 访问已映射但未分配物理页的地址 → **Minor Fault**（分配零页或从 page cache 获取）
3. 访问已被换出到交换分区的页 → **Major Fault**（从磁盘读回）
4. 写入 COW 共享页 → **COW Fault**（本内核重点实现）

**处理流程**（本内核 `handle_pgfault`）：
```
验证地址在 VmRegion 内 → 检查权限匹配 → 写操作触发 COW 处理 → 分配/替换物理页
```

#### 16. 页面回收与交换

当内存不足时，内核需要回收页面：

**回收优先级**（从高到低）：
1. 未修改的文件页缓存（直接丢弃，需要时从文件重读）
2. 已修改的文件页缓存（先写回再丢弃）
3. 匿名页（如堆、栈）→ 写入 swap 分区
4. 内核 slab 缓存（收缩 slab）

**LRU (Least Recently Used)**：维持活跃链表和不活跃链表，优先回收不活跃链表中的页面。本内核的 `PageCache::evict_lru` 实现了简化版 LRU 逐出。

---

### 四、文件系统

#### 17. VFS 的设计目标

VFS (Virtual File System) 是 Linux 的**文件系统抽象层**：

- **统一接口**：open/read/write/close 对所有文件系统类型使用相同的系统调用
- **多态实现**：每个文件系统提供自己的 `file_operations`、`inode_operations`
- **统一命名空间**：所有文件系统挂载在同一个目录树下

本内核中通过 `FLike` enum + trait-like 方法模拟了这一设计。

#### 18. 文件描述符的本质

文件描述符（fd）是一个**整数索引**，指向进程 fd 表中一个条目，该条目指向内核的**打开文件描述**（file description），后者包含：
- 读写偏移量
- 打开标志（O_RDONLY/O_WRONLY/O_RDWR）
- 指向 inode 的指针

**fork/exec 与 fd**：
- fork：子进程获得父进程 fd 表的副本（dup），共享同一文件描述（共享偏移量）
- exec：带有 `FD_CLOEXEC` 标志的 fd 自动关闭

#### 19. 管道 (Pipe)

**匿名管道**（`pipe()` 创建）：
- 单向数据流：写端→读端
- 内核缓冲区（通常 64KB）
- 写端关闭时，读端读到 EOF
- 写满时，写者阻塞（或非阻塞模式下返回 EAGAIN）

**命名管道 (FIFO)**：在文件系统中有一个名字，不相关的进程可以打开它进行通信。

#### 20. epoll: LT vs ET

**LT (Level-Triggered，电平触发，默认)**：
- 只要缓冲区有数据，每次 `epoll_wait` 都返回
- 编程简单，适合大多数场景

**ET (Edge-Triggered，边缘触发)**：
- 仅在缓冲区从"空→有数据"或从"不可写→可写"时返回**一次**
- 减少 `epoll_wait` 调用次数，性能更好
- 要求一次性读完/写完所有数据（配合非阻塞 IO）
- 适合高并发服务器（nginx 的默认模式）

**EPOLLONESHOT**：触发一次后自动移除监控，直到通过 `EPOLL_CTL_MOD` 重新注册。

#### 21. 阻塞 I/O vs 非阻塞 I/O

| 模式 | read() 行为 | write() 行为 |
|------|------------|------------|
| **阻塞** (默认) | 无数据时阻塞直到有数据或 EOF | 缓冲区满时阻塞直到可写 |
| **非阻塞** (O_NONBLOCK) | 无数据时立即返回 EAGAIN | 缓冲区满时立即返回 EAGAIN |

非阻塞 I/O 通常配合 I/O 多路复用（select/poll/epoll）使用：先用 epoll 等待 fd 就绪，再用非阻塞 read/write 操作数据。

---

### 五、同步与并发

#### 22. 自旋锁 vs 互斥锁

| 特性 | 自旋锁 (Spin Lock) | 互斥锁 (Mutex) |
|------|-------------------|---------------|
| 等待方式 | 忙等待（CPU 空转） | 睡眠（放弃 CPU） |
| 适用场景 | 临界区很短（几微秒） | 临界区较长（毫秒级） |
| 持有期要求 | 不可睡眠，不可被抢占 | 可以睡眠 |
| 开销 | 获取释放很快，但等待浪费 CPU | 获取释放有系统调用开销 |
| 中断上下文 | 可以使用 | 不可使用（会睡眠） |

**为什么自旋锁持有者不能被抢占？**如果持有自旋锁的线程被抢占，其他 CPU 上的线程可能长时间自旋等待该锁，造成严重的性能浪费。

#### 23. 递归锁 (Reentrant Lock)

**使用场景**：当同一个线程可能通过不同调用路径多次获取同一锁时。例如内核中：系统调用入口获取 GKL → 调用文件系统代码 → 触发缓存刷新 → 再次尝试获取 GKL。

**本内核 KernLock 的实现**：
- `owner_tid` 记录持有者线程 ID（而非锁 ID），用于同线程可重入判断
- 不同 `id` 值表示不同的调用上下文（如"tick" vs "sync_all"），但仍属于同一线程
- `holder_stack` 确保 enter/leave 正确配对

#### 24. 信号量的 P/V 操作

- **P 操作** (Proberen, 荷兰语"尝试")：`while (count <= 0) wait(); count--;`
- **V 操作** (Verhogen, 荷兰语"增加")：`count++; wake_up_waiters();`

**计数信号量 vs 二进制信号量**：
- 计数信号量（count >= 0）：允许多个线程同时访问（如限制最大连接数）
- 二进制信号量（count ∈ {0,1}）：等价于互斥锁（但无所有权概念）

#### 25. futex 的"混合"设计

futex = **Fast Userspace muTEX**

**关键洞察**：大多数情况下锁没有竞争（uncontended），可以直接在用户态通过原子 CAS 完成。

```
// 用户态加锁（无竞争路径）
while (atomic_cas(&lock, 0, 1) != 0) {
    // 竞争路径 → 进入内核
    futex_wait(&lock, 1);  // 在内核中睡眠
}
// 用户态解锁
atomic_store(&lock, 0);
futex_wake(&lock, 1);  // 唤醒等待者
```

**好处**：无竞争时零系统调用，有竞争时才进入内核。相比传统 mutex（每次 lock/unlock 都涉及系统调用），性能提升显著。

#### 26. 死锁的四个必要条件

1. **互斥**：资源只能被一个进程独占
2. **持有并等待**：进程持有资源的同时等待其他资源
3. **不可剥夺**：已分配的资源不能被强制收回
4. **循环等待**：存在 P₁ → P₂ → ... → Pₙ → P₁ 的等待环

**预防**：破坏任一条件。如：一次性分配所有资源（破坏"持有并等待"），资源排序（破坏"循环等待"）。

#### 27. 优先级反转

**场景**：
1. 低优先级任务 L 持有锁
2. 高优先级任务 H 等待该锁
3. 中优先级任务 M 持续运行（抢占 L，但不持有锁）
4. H 被 M 无限期阻塞——高优先级任务等待低优先级任务

**解决方案 — Priority Inheritance (PI)**：
- 当 H 等待 L 持有的锁时，**临时**将 L 的优先级提升到 H 的优先级
- L 完成后释放锁，优先级恢复
- futex 的 `FUTEX_LOCK_PI` / `FUTEX_UNLOCK_PI` 实现了内核辅助的优先级继承

---

### 六、IPC

#### 28. System V 信号量 vs POSIX 信号量

| 特性 | System V 信号量 | POSIX 信号量 |
|------|----------------|-------------|
| API | semget/semop/semctl | sem_init/sem_wait/sem_post |
| 命名 | key (整数) | 名字（如 "/mysem"） |
| 集合操作 | 支持（原子操作多个信号量） | 不支持（每次操作一个） |
| SEM_UNDO | 支持（进程退出自动撤销） | 不支持 |
| 接口风格 | 复杂的 ioctl 风格 | 简洁的函数风格 |
| 持久性 | 内核持久（需显式删除） | 命名信号量持久，匿名信号量随进程 |

#### 29. 共享内存

共享内存是**最快的 IPC 方式**，因为数据不需要在内核和用户空间之间复制。

**使用流程**：
1. shmget(key, size, flags)：创建/获取共享内存段
2. shmat(shmid, addr, flags)：将共享内存映射到进程地址空间
3. 直接读写共享内存（如同普通内存）
4. shmdt(addr)：解除映射

**同步**：共享内存本身不提供同步机制，通常配合信号量使用（生产者-消费者模型）。

#### 30. 各种 IPC 方式的比较

| IPC 方式 | 速度 | 数据量 | 同步 | 持久性 |
|---------|------|--------|------|--------|
| 信号 (Signal) | 慢 | 极小（信号编号） | 异步 | 易丢失 |
| 管道 (Pipe) | 中等 | 流式 | 内置同步 | 随进程 |
| 消息队列 | 中等 | 结构化消息 | 可选阻塞 | 内核持久 |
| 共享内存 | 最快 | 任意大小 | 需额外同步 | 内核持久 |
| Socket | 慢-中等 | 流式/数据报 | 可选阻塞 | 可跨网络 |
| 信号量 | N/A | 不传输数据 | 专用于同步 | 内核持久 |

---

### 七、信号

#### 31. 信号的三个处理阶段

1. **生成**：`kill()` 系统调用或内核事件（如 SIGCHLD）产生信号，加入目标进程的 `sig_queue`
2. **挂起**：信号在队列中等待，直到被递送。如果被 `sig_mask` 阻塞，则保持挂起
3. **递送**：进程被调度运行时，内核检查 `pending & !blocked`，将信号递送给进程，执行对应的处理函数（默认/忽略/用户注册）

#### 32. 可靠信号 vs 不可靠信号

- **不可靠信号**（SIG 1-31）：挂起期间如果再次到达**可能丢失**（队列只记录"存在"而非"个数"）
- **可靠信号**（SIGRTMIN-SIGRTMAX，实时信号）：排队递送，不会丢失。支持携带附加数据（sigqueue 发送）

本内核中使用 `VecDeque` 作为信号队列，理论上对于所有信号都可以排队（简化实现）。

#### 33. 信号处理的三种方式

- **SIG_DFL (0)**：默认动作。分为 Term（终止）、Core（终止+core dump）、Stop（暂停）、Cont（继续）、Ign（忽略）
- **SIG_IGN (1)**：忽略。信号被静默丢弃
- **自定义处理函数**：用户注册的函数地址，收到信号时调用

**SIGKILL/SIGSTOP 的不可捕获性**保证了系统管理员始终可以终止或暂停任何进程。

#### 34. 信号安全函数

信号处理函数中只能调用**异步信号安全（async-signal-safe）**的函数。这些函数保证可重入（reentrant）或不会被信号中断。

不可调用的例子：`malloc`、`printf`（内部使用全局锁）、`fopen`

可调用的例子：`write`（系统调用）、`_exit`、`signal`、`sem_post`

**原因**：信号处理函数可能在任意时刻打断主程序的执行（包括在 malloc 内部持有锁时），如果处理函数再次调用 malloc，会导致死锁。

---

### 八、系统调用

#### 35. 系统调用的完整路径

```
用户态                          内核态
  │                               │
  │  syscall(READ, fd, buf, n)    │
  ├─ mov $0, %rax  (系统调用号)    │
  ├─ mov fd, %rdi                 │
  ├─ ...                          │
  ├─ syscall 指令                  │
  │  └── CPU 切换到内核态 ────────→ │
  │      (MSR_LSTAR → entry)     │ entry_SYSCALL_64:
  │                               ├─ swapgs (保存用户GS)
  │                               ├─ 保存所有寄存器到 pt_regs
  │                               ├─ 调用 do_syscall_64(regs)
  │                               │   └─ dispatch_syscall(nr, a0..a5)
  │                               │       └─ sys_read(fd, buf, n)
  │                               ├─ 恢复寄存器
  │                               ├─ sysretq ←────────┐
  │  ←────── CPU 切换回用户态 ────┘                   │
  │  返回 read() 的返回值                             │
```

本内核中 `dispatch_syscall()` 模拟了步骤中的系统调用分发环节。

#### 36. copy_from_user 的必要性

**不能直接解引用用户态指针的原因**：
1. **安全性**：用户态可能传递内核地址（`KERN_BASE` 以上），直接解引用会崩溃或导致权限提升
2. **正确性**：用户地址可能未映射，需要触发缺页异常（内核需设置特殊的缺页处理）
3. **SMAP (Supervisor Mode Access Prevention)**：Intel 硬件特性，禁止内核直接访问用户态内存（必须通过 `copy_from_user` 等函数，它们会临时关闭 SMAP）

本内核中 `check_access(addr, len)` 首先确保地址在 `KERN_BASE` 以下，然后通过 `ensure_user_range` 逐页确认映射存在。

#### 37. 系统调用号的设计

- 系统调用号是**稳定 ABI**，一旦分配永不改变（Linux 的铁律："we never break userspace"）
- 通过编号而非函数名匹配，避免了符号解析开销
- 不同架构有不同的系统调用号和 ABI（x86_64 使用 `syscall` 指令，传参用 rdi/rsi/rdx/r10/r8/r9，系统调用号在 rax）

---

### 九、中断与异常

#### 38. 中断 vs 异常 vs 系统调用

| 类型 | 触发源 | 同步/异步 | 示例 |
|------|--------|-----------|------|
| **中断 (Interrupt)** | 外部硬件 | 异步 | 定时器、网卡、键盘 |
| **异常 (Exception)** | 当前执行的指令 | 同步 | 缺页、除零、非法指令 |
| **系统调用 (Trap)** | 软件 `syscall` 指令 | 同步 | read、write、fork |

- **异步**：与当前执行的指令无关，可以在任意时刻发生
- **同步**：由特定指令引起，在指令边界处处理

本内核 `TrapCtl::dispatch_vector`：
- 向量 0-7：硬件中断 → `in_irq = true`（禁止缺页）
- 向量 8-15：软件中断/系统调用 → `in_irq = false`（允许缺页）
- 向量 14：缺页异常本身 → `in_irq = true`（防止嵌套缺页）

#### 39. 定时器中断的作用

每次定时器中断，内核执行 `tick()`：

1. **更新时间**：递增 `CLK` 和 `CLK_ALL`
2. **调度检查**：当前任务时间片是否耗尽，是否需要抢占
3. **缓存写回**：遍历所有 BlockCache 链，将脏块写回磁盘
4. **定时器处理**：推进 TimerWheel，触发到期定时器的回调

定时器频率是重要的设计参数：
- 高频率（如 1000Hz）：响应性好，但开销大
- 低频率（如 100Hz）：开销小，但调度粒度粗

本内核使用 `TIMER_TICK_HZ = 100`（10ms tick 间隔）。

#### 40. 上下文切换的完整过程

```
schedule() 被调用：
  1. 保存当前任务上下文：
     - 通用寄存器 → thd_ctx.uctx.r[]
     - IP/RIP → thd_ctx.uctx.ip
     - FLAGS → thd_ctx.uctx.flags
  2. 更新当前任务的 vruntime
  3. 选择下一个任务（CFS 选 vruntime 最小者）
  4. 切换地址空间：
     - 更新 CR3（页表基址寄存器）
     - 可能需要 TLB 刷新（通过 ASID 避免）
  5. 恢复下一个任务的上下文：
     - 加载寄存器、IP、FLAGS
  6. 返回到新任务的执行流
```

**ASID (Address Space ID)**：为每个地址空间分配唯一 ID，TLB 条目带有 ASID 标签。切换地址空间时不需要全量刷新 TLB——这是一个重要的性能优化。

---

### 十、安全与权限

#### 41. Capability 安全模型

传统 Unix 权限：root（UID=0）拥有所有权限，是一个**全或无**的模型。

**Capability 模型**：将 root 权限细分为多种独立的能力：
- `CAP_KILL`：发送信号给任意进程
- `CAP_NET_BIND`：绑定 <1024 的端口
- `CAP_SYS_ADMIN`：系统管理操作
- `CAP_SYS_PTRACE`：ptrace 任意进程

本内核中 `CapSet` 实现了三个集合：
- **Permitted**：进程被允许拥有的能力
- **Effective**：当前生效的能力（用于权限检查）
- **Ambient**：跨 exec 保留的能力

一个网络服务器可以只拥有 `CAP_NET_BIND` 而非完整的 root 权限——**最小权限原则**。

#### 42. 用户态与内核态的隔离

`check_access` 的实现反映了**地址空间隔离**的核心：

```rust
pub fn check_access(addr: usize, len: usize) -> bool {
    addr < KERN_BASE && len <= KERN_BASE - addr
}
```

`KERN_BASE = 0xFFFF_FFFF_8000_0000` 是 x86_64 上典型的**内核-用户空间分界线**。

x86_64 虚拟地址空间布局：
```
0x0000_0000_0000_0000  ─┬─ 用户空间（低 128TB）
                        │
0x0000_7FFF_FFFF_FFFF  ─┼─ 非规范地址空间空洞
                        │
0xFFFF_8000_0000_0000  ─┼─ 内核空间（高 128TB）
                        │
0xFFFF_FFFF_FFFF_FFFF  ─┘
```

**硬件保护**：页表项中的 U/S 位决定了用户态是否能访问该页。即使通过漏洞修改了 `check_access`，硬件依然阻止用户态访问内核页。

#### 43. CLOEXEC 的安全意义

`close-on-exec` 标志解决一个经典安全问题：

**攻击场景**：父进程打开一个特权文件，然后 fork+exec 执行一个不受信任的程序。由于 fd 在 exec 后默认保留，子进程可以通过继承的 fd 访问特权文件。

**解决方案**：打开文件时设置 `O_CLOEXEC`，exec 时内核自动关闭带有此标志的 fd。

```rust
// do_exec 中的处理
for fd in fds_with_cloexec {
    task.files.lock().unwrap().remove(&fd);
}
```

---

*本文档基于 `/home/entong/OS-Homework/kernel/src/kernel/` 下全部 27 个模块的源码分析，涵盖模拟内核的实现细节和操作系统基础概念。*
