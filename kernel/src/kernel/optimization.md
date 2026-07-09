# Optimization

1. **KernLock holder 改固定栈** — `AtomicUsize` → `[AtomicUsize; 16]`，用 depth 当栈指针，`const fn` 兼容，GKL 直接 `static` 初始化。
2. **KernLock leave 加 id** — `leave(id)` 校验 `stack[depth-1] == id`，不匹配打印 MISMATCH。
3. **PgFrame 加 frame_id** — 新增 `frame_id: AtomicUsize` 和 `phys_addr()` 方法，`new(frame_id)` / `with_rc(frame_id, n)` 绑定帧编号。
4. **PgFrame rc 初始化为 0** — `new()` rc=0，挂入 cow_pages 时 `with_rc(frame_id, 1)` 表示持有一个引用。
5. **handle_cow_fault 修复** — ① rc≤1 返回物理地址而非虚拟页号；② COW 新帧绑定 frame_id；③ fork_from 传入父帧 frame_id。
6. **Task 加 AddrSpace** — `addr_space: Mutex<AddrSpace>`，`do_fork` 调用 `AddrSpace::fork_from`。
7. **handle_pgfault 补全** — VmMap::find 查映射 → 权限检查 → 写操作走 handle_cow_fault → 读操作直接成功。
8. **syscall 加 ensure_user_range** — 所有访问用户内存的 syscall（READ/WRITE/OPEN/EXEC/Wait等）在 check_access 后逐页调 handle_pgfault。
9. **SimpleFS 文件系统** — 以 inode 树组织，支持 create_file / create_dir / unlink / lookup / read_data / write_data / truncate / read_dir。
10. **FHandle 加 ino 字段** — `pub ino: usize` 直接存 inode 编号，SYS_OPEN 设 `fh.ino`，READ/WRITE 用 `fh.ino` 调文件系统。
11. **Disk 加真实存储** — `blocks: Mutex<BTreeMap<usize, [u8; 512]>>`，read_block / write_block / read_block_n 改为读写真实数据。
12. **BlockCache 连 Disk** — `fetch()` 增加 `disk: &Disk` 参数，缓存未命中时从 Disk 读取。
13. **SimpleFS 走 BlockCache→Disk** — Inode 改为 `blocks: Vec<usize>`（磁盘块号列表），读写通过 BlockCache::fetch → Disk 持久化。
14. **SYS_OPEN/READ/WRITE 重写** — 通过 SimpleFS 打开/创建文件、读写数据。
15. **SYS_CLOSE 修复** — 直接从 Task.files 删除 fd。
16. **加 SYS_MKDIR / SYS_UNLINK** — 文件创建目录、删除文件。
17. **read_data 返回 Vec<u8>** — 去掉无用的 `buf: &mut [u8]` 参数，直接返回读到的数据。
18. **BlockCache 加 put 方法** — 写操作修改缓存并标脏，不再绕过缓存直接写磁盘。
19. **tick / sync_all 写回磁盘** — 脏块在 `modified` 标志清除前调用 `disk.write_block` 持久化。
20. **Disk 保留故障注入 + 0xAA** — `read_block` 保留重试循环，未初始化块填 `0xAA`，已写过块返回真实数据。
