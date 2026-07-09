//! 内核全局常量定义。
//!
//! 本模块包含仿真内核中使用的所有常量，包括：
//! - 内存布局常量（页面大小、内核基址、物理偏移等）
//! - 文件系统常量（fcntl 操作码、文件打开标志）
//! - 终端 IO 控制常量（tcgetattr / tcsetattr 等）
//! - 虚拟内存标志位（读/写/执行/共享等）
//! - 进程权限（capability）标志位
//! - 内存区域（zone）常量
//! - 调度器常量（优先级范围、调度策略）
//! - Slab 分配器常量
//! - 信号常量
//! - 定时器常量
//! - 网络套接字常量
//! - 系统调用编号

// ── 内存布局 ──

/// 页面大小（字节）。
pub const PAGE_SZ: usize = 4096;
/// 最大进程数。
pub const N_PROC: usize = 256;
/// 物理页帧总数。
pub const N_FRAMES: usize = 65536;
/// 内核虚拟地址基址。
pub const KERN_BASE: usize = 0xFFFF_FFFF_8000_0000;
/// 物理地址直接映射区的起始虚拟地址。
pub const PHYS_OFF: usize = 0xFFFF_FFFF_0000_0000;
/// 物理内存起始偏移。
pub const MEM_OFF: usize = 0x8000_0000;
/// 内核堆大小。
pub const KHEAP_SZ: usize = 0x800000;
/// 块缓存链数量。
pub const N_CHAINS: usize = 64;
/// 环形缓冲区容量。
pub const RBUF_CAP: usize = 256;
/// 通用寄存器数量。
pub const N_REGS: usize = 16;
/// 最大挂载深度。
pub const MNT_DEPTH: usize = 8;
/// 最大 CPU 数量。
pub const MAX_CPU: usize = 8;
/// 内核栈大小。
pub const KSTK_SZ: usize = 0x4000;
/// 用户栈起始地址。
pub const USR_STK_OFF: usize = 0x7FFF_0000;
/// 用户栈大小。
pub const USR_STK_SZ: usize = 0x10000;
/// 每 tick 的微秒数。
pub const USEC_TICK: usize = 1000;
/// 符号链接跟随深度限制。
pub const FOLLOW_LIM: usize = 3;
/// 系统启动时的初始时间戳（epoch）。
pub const BOOT_EPOCH: usize = 0;

// ── 文件控制（fcntl）操作码 ──

/// fcntl: 复制文件描述符到最小可用编号。
pub const F_DUPFD: usize = 0;
/// fcntl: 获取文件描述符标志（close-on-exec）。
pub const F_GETFD: usize = 1;
/// fcntl: 设置文件描述符标志。
pub const F_SETFD: usize = 2;
/// fcntl: 获取文件状态标志（O_NONBLOCK, O_APPEND等）。
pub const F_GETFL: usize = 3;
/// fcntl: 设置文件状态标志。
pub const F_SETFL: usize = 4;
/// fcntl: 获取文件锁。
pub const F_GETLK: usize = 5;
/// fcntl: 设置文件锁（非阻塞）。
pub const F_SETLK: usize = 6;
/// fcntl: 设置文件锁（阻塞等待）。
pub const F_SETLKW: usize = 7;
/// close-on-exec 标志：exec 后自动关闭该 fd。
pub const FD_CLOEXEC: usize = 1;
/// fcntl: 复制文件描述符并设置 close-on-exec。
pub const F_DUPFD_CLOEXEC: usize = 1030;
/// 文件以非阻塞模式打开。
pub const O_NONBLOCK: usize = 0o4000;
/// 文件以追加模式打开。
pub const O_APPEND: usize = 0o2000;
/// 文件以 close-on-exec 模式打开。
pub const O_CLOEXEC: usize = 0o2000000;
/// 不跟随符号链接。
pub const AT_NOFOLLOW: usize = 0x100;

// ── 终端 IO 控制（ioctl）命令 ──

/// ioctl: 获取终端属性（termios）。
pub const TCGETS: usize = 0x5401;
/// ioctl: 设置终端属性。
pub const TCSETS: usize = 0x5402;
/// ioctl: 获取前台进程组 ID。
pub const TIOCGPGRP: usize = 0x540F;
/// ioctl: 设置前台进程组 ID。
pub const TIOCSPGRP: usize = 0x5410;
/// ioctl: 获取终端窗口大小。
pub const TIOCGWINSZ: usize = 0x5413;
/// ioctl: 清除 close-on-exec 标志。
pub const FIONCLEX: usize = 0x5450;
/// ioctl: 设置 close-on-exec 标志。
pub const FIOCLEX: usize = 0x5451;
/// ioctl: 设置/获取非阻塞模式。
pub const FIONBIO: usize = 0x5421;

// ── ELF 辅助向量类型 ──

/// 辅助向量：程序头表地址。
pub const AT_PHDR: u8 = 3;
/// 辅助向量：程序头表条目大小。
pub const AT_PHENT: u8 = 4;
/// 辅助向量：程序头表条目数量。
pub const AT_PHNUM: u8 = 5;
/// 辅助向量：页面大小。
pub const AT_PAGESZ: u8 = 6;
/// 辅助向量：动态链接器基址。
pub const AT_BASE: u8 = 7;
/// 辅助向量：程序入口地址。
pub const AT_ENTRY: u8 = 9;

// ── 终端本地模式标志（termios l_flag） ──

/// 启用信号处理（Ctrl+C 等）。
pub const LM_ISIG: u32 = 0o000001;
/// 启用规范模式（行缓冲）。
pub const LM_ICANON: u32 = 0o000002;
/// 输入字符回显。
pub const LM_ECHO: u32 = 0o000010;
/// 擦除字符回显（退格可见效果）。
pub const LM_ECHOE: u32 = 0o000020;
/// KILL 字符回显。
pub const LM_ECHOK: u32 = 0o000040;
/// 换行符回显。
pub const LM_ECHONL: u32 = 0o000100;
/// 禁止在中断后 flush 缓冲区。
pub const LM_NOFLSH: u32 = 0o000200;
/// 后台进程写入终端时发送 SIGTTOU。
pub const LM_TOSTOP: u32 = 0o000400;
/// 启用扩展输入字符处理。
pub const LM_IEXTEN: u32 = 0o100000;
/// 终端输出大写映射（已废弃）。
pub const LM_XCASE: u32 = 0o000004;
/// 控制字符回显为 `^X` 形式。
pub const LM_ECHOCTL: u32 = 0o001000;
/// 擦除字符回显为可见形式。
pub const LM_ECHOPRT: u32 = 0o002000;
/// KILL 字符回显。
pub const LM_ECHOKE: u32 = 0o004000;
/// 输出被 flush 到终端。
pub const LM_FLUSHO: u32 = 0o010000;
/// 重新打印未读输入。
pub const LM_PENDIN: u32 = 0o040000;
/// 启用外部进程处理（类似于 ptrace 的终端模式）。
pub const LM_EXTPROC: u32 = 0o200000;

// ── 虚拟内存区域（VMA）标志 ──

/// 页可读。
pub const VM_READ: u32 = 0x01;
/// 页可写。
pub const VM_WRITE: u32 = 0x02;
/// 页可执行。
pub const VM_EXEC: u32 = 0x04;
/// 页可在进程间共享。
pub const VM_SHARED: u32 = 0x08;
/// 栈区域，向下增长。
pub const VM_GROWSDOWN: u32 = 0x10;
/// fork 时不要复制此区域。
pub const VM_DONTCOPY: u32 = 0x20;
/// 大页（HugeTLB）映射。
pub const VM_HUGETLB: u32 = 0x40;
/// PFN 直接映射（无 struct page）。
pub const VM_PFNMAP: u32 = 0x80;

// ── 进程权限（Capability）标志 ──

/// 允许修改文件所有者。
pub const CAP_CHOWN: u32 = 0;
/// 允许发送任意信号。
pub const CAP_KILL: u32 = 5;
/// 允许修改 UID。
pub const CAP_SETUID: u32 = 7;
/// 允许修改 GID。
pub const CAP_SETGID: u32 = 6;
/// 允许绑定特权端口。
pub const CAP_NET_BIND: u32 = 10;
/// 允许使用原始套接字。
pub const CAP_NET_RAW: u32 = 13;
/// 允许执行系统管理操作。
pub const CAP_SYS_ADMIN: u32 = 21;
/// 允许跟踪任意进程。
pub const CAP_SYS_PTRACE: u32 = 19;
/// exec 时可继承的 capability 掩码。
pub const INHERITABLE_MASK: u64 = 0x0000_00FF_FFFF_FFFF;

// ── 内存区域（Zone）类型 ──

/// DMA 区域编号（低 16MB）。
pub const ZONE_DMA: usize = 0;
/// 普通内存区域编号。
pub const ZONE_NORMAL: usize = 1;
/// 高端内存区域编号。
pub const ZONE_HIGH: usize = 2;
/// 内存区域总数。
pub const N_ZONES: usize = 3;

// ── 调度器常量 ──

/// 最低优先级（最高优先）。
pub const PRIO_MIN: i32 = -20;
/// 最高优先级（最低优先）。
pub const PRIO_MAX: i32 = 19;
/// 默认优先级。
pub const PRIO_DEFAULT: i32 = 0;
/// 普通调度策略（CFS）。
pub const SCHED_NORMAL: u8 = 0;
/// 先入先出实时调度。
pub const SCHED_FIFO: u8 = 1;
/// 时间片轮转实时调度。
pub const SCHED_RR: u8 = 2;
/// 批处理调度策略。
pub const SCHED_BATCH: u8 = 3;

// ── Slab 分配器常量 ──

/// slab 对象最小大小。
pub const SLAB_OBJ_MIN: usize = 8;
/// slab 对象最大大小。
pub const SLAB_OBJ_MAX: usize = 2048;
/// slab 对象对齐粒度。
pub const SLAB_ALIGN: usize = 8;

// ── 信号常量 ──

/// 信号编号的数量（标准 + 实时）。
pub const NSIG: u32 = 64;
/// 信号的默认处理动作。
pub const SIG_DFL: usize = 0;
/// 忽略该信号。
pub const SIG_IGN: usize = 1;
/// SIGKILL 信号编号（不可被阻塞或忽略）。
pub const SIGKILL: u32 = 9;
/// SIGSTOP 信号编号（不可被阻塞或忽略）。
pub const SIGSTOP: u32 = 19;
/// SIGCHLD 信号编号（子进程状态变化）。
pub const SIGCHLD: u32 = 17;
/// SIGUSR1 信号编号（用户自定义信号1）。
pub const SIGUSR1: u32 = 10;
/// SIGUSR2 信号编号（用户自定义信号2）。
pub const SIGUSR2: u32 = 12;
/// SIGALRM 信号编号（定时器到期）。
pub const SIGALRM: u32 = 14;

// ── 定时器常量 ──

/// 时间轮槽位数。
pub const TIMER_WHEEL_SIZE: usize = 256;
/// 定时器中断频率（tick/秒）。
pub const TIMER_TICK_HZ: usize = 100;

// ── 网络套接字常量 ──

/// 流式套接字（TCP）。
pub const SOCK_STREAM: u32 = 1;
/// 数据报套接字（UDP）。
pub const SOCK_DGRAM: u32 = 2;
/// 原始套接字。
pub const SOCK_RAW: u32 = 3;
/// IPv4 地址族。
pub const AF_INET: u32 = 2;
/// IPv6 地址族。
pub const AF_INET6: u32 = 10;
/// Unix 域套接字地址族。
pub const AF_UNIX: u32 = 1;

// ── 系统调用编号 ──

/// sys_read: 从文件描述符读取数据。
pub const SYS_READ: usize = 0;
/// sys_write: 向文件描述符写入数据。
pub const SYS_WRITE: usize = 1;
/// sys_open: 打开或创建文件。
pub const SYS_OPEN: usize = 2;
/// sys_close: 关闭文件描述符。
pub const SYS_CLOSE: usize = 3;
/// sys_stat: 获取文件状态信息。
pub const SYS_STAT: usize = 4;
/// sys_fstat: 通过文件描述符获取文件状态。
pub const SYS_FSTAT: usize = 5;
/// sys_mmap: 映射文件或匿名内存到进程地址空间。
pub const SYS_MMAP: usize = 9;
/// sys_munmap: 取消内存映射。
pub const SYS_MUNMAP: usize = 11;
/// sys_brk: 调整进程数据段（堆）大小。
pub const SYS_BRK: usize = 12;
/// sys_ioctl: 输入输出控制操作。
pub const SYS_IOCTL: usize = 16;
/// sys_pipe: 创建管道。
pub const SYS_PIPE: usize = 22;
/// sys_dup: 复制文件描述符。
pub const SYS_DUP: usize = 32;
/// sys_dup2: 将文件描述符复制到指定编号。
pub const SYS_DUP2: usize = 33;
/// sys_fork: 创建子进程（复制当前进程）。
pub const SYS_FORK: usize = 57;
/// sys_exec: 执行新程序。
pub const SYS_EXEC: usize = 59;
/// sys_exit: 终止当前进程。
pub const SYS_EXIT: usize = 60;
/// sys_wait4: 等待子进程状态变化。
pub const SYS_WAIT4: usize = 61;
/// sys_kill: 向进程发送信号。
pub const SYS_KILL: usize = 62;
/// sys_fcntl: 文件描述符控制操作。
pub const SYS_FCNTL: usize = 72;
/// sys_getpid: 获取当前进程 ID。
pub const SYS_GETPID: usize = 39;
/// sys_getppid: 获取父进程 ID。
pub const SYS_GETPPID: usize = 110;
/// sys_setpgid: 设置进程组 ID。
pub const SYS_SETPGID: usize = 109;
/// sys_getpgid: 获取进程组 ID。
pub const SYS_GETPGID: usize = 121;
/// sys_setsid: 创建新会话。
pub const SYS_SETSID: usize = 112;
/// sys_epoll_create: 创建 epoll 实例。
pub const SYS_EPOLL_CREATE: usize = 213;
/// sys_epoll_ctl: 控制 epoll 实例（添加/修改/删除监控）。
pub const SYS_EPOLL_CTL: usize = 233;
/// sys_epoll_wait: 等待 epoll 事件。
pub const SYS_EPOLL_WAIT: usize = 232;
/// sys_clock_gettime: 获取时钟时间。
pub const SYS_CLOCK_GETTIME: usize = 228;
/// sys_sigaction: 设置信号处理动作。
pub const SYS_SIGACTION: usize = 13;
/// sys_sigprocmask: 修改信号掩码。
pub const SYS_SIGPROCMASK: usize = 14;
/// sys_futex: 快速用户态互斥锁操作。
pub const SYS_FUTEX: usize = 202;
/// sys_mkdir: 创建目录。
pub const SYS_MKDIR: usize = 83;
/// sys_unlink: 删除文件。
pub const SYS_UNLINK: usize = 87;
/// sys_truncate: 截断文件。
pub const SYS_TRUNCATE: usize = 92;
/// sys_getdents: 读取目录项。
pub const SYS_GETDENTS: usize = 217;

/// IO 队列最大深度。
pub const IOQUEUE_DEPTH: usize = 128;
