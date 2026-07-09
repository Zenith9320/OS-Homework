//! 简单内存文件系统。inode 树组织，root 是 inode 0。
//! 数据通过在磁盘块上模拟存储来实现持久化（通过 BlockCache 和 Disk）。

use std::sync::Mutex;
use std::collections::BTreeMap;
use std::time::Duration;
use super::cache::BlockCache;
use super::io::Disk;

/// 磁盘块大小（512 字节）。
const BLOCK_SZ: usize = 512;

/// inode 类型：文件或目录。
#[derive(Clone, PartialEq)]
pub enum InodeType { File, Directory }

/// 文件系统节点，可以表示文件或目录。
///
/// - 文件：`blocks` 存磁盘块号列表，`children` 空闲不用。
/// - 目录：`children` 存 `文件名 → inode 编号` 映射，`blocks` 空闲不用。
pub struct Inode {
    /// inode 编号
    pub ino: usize,
    /// 文件还是目录
    pub inode_type: InodeType,
    /// 磁盘块号列表（文件），每个块 512 字节
    pub blocks: Mutex<Vec<usize>>,
    /// 文件大小（字节）
    pub size: Mutex<usize>,
    /// 目录条目：文件名 → inode 编号
    pub children: Mutex<BTreeMap<String, usize>>,
}

impl Inode {
    /// 创建一个新的文件 inode。
    fn new_file(ino: usize) -> Self {
        Self { ino, inode_type: InodeType::File, blocks: Mutex::new(Vec::new()), size: Mutex::new(0), children: Mutex::new(BTreeMap::new()) }
    }
    /// 创建一个新的目录 inode。
    fn new_dir(ino: usize) -> Self {
        Self { ino, inode_type: InodeType::Directory, blocks: Mutex::new(Vec::new()), size: Mutex::new(0), children: Mutex::new(BTreeMap::new()) }
    }
}

/// 路径查找结果。
///
/// `ino == usize::MAX` 表示目标不存在，此时 `parent_ino` 和 `name`
/// 可用来在该位置创建新文件或目录。
pub struct PathLookup {
    /// 目标 inode（usize::MAX 表示不存在）
    pub ino: usize,
    /// 父目录 inode
    pub parent_ino: usize,
    /// 文件名（最后一段）
    pub name: String,
}

/// 简单文件系统，以 inode 树组织，root 是 inode 0。
///
/// 文件数据通过 `blocks` → BlockCache → Disk 实现持久化；
/// 目录的 `children` 目前是纯内存结构。
pub struct SimpleFS {
    /// inode 表：ino → Inode
    pub inodes: Mutex<BTreeMap<usize, Inode>>,
    /// 下一个可用的 inode 编号
    next_ino: Mutex<usize>,
    /// 下一个可用的磁盘块号
    next_blk: Mutex<usize>,
}

impl SimpleFS {
    /// 创建空文件系统，自动建立根目录 /（ino=0）。
    pub fn new() -> Self {
        let mut inodes = BTreeMap::new();
        inodes.insert(0, Inode::new_dir(0));
        Self { inodes: Mutex::new(inodes), next_ino: Mutex::new(1), next_blk: Mutex::new(0) }
    }

    /// 分配一个新的 inode 编号。
    fn alloc_ino(&self) -> usize {
        let mut n = self.next_ino.lock().unwrap();
        let ino = *n; *n += 1; ino
    }

    /// 分配一个新的磁盘块号。
    fn alloc_block(&self) -> usize {
        let mut n = self.next_blk.lock().unwrap();
        let b = *n; *n += 1; b
    }

    /// 解析路径，返回目标 inode 编号。路径不存在则报错。
    pub fn resolve(&self, path: &str) -> Result<usize, &'static str> {
        Ok(self.lookup(path)?.ino)
    }

    /// 逐段从根目录遍历路径。最后一段存在则返回其 ino；
    /// 不存在则返回 `ino=MAX` 及父目录信息（供 create 使用）。
    pub fn lookup(&self, path: &str) -> Result<PathLookup, &'static str> {
        let inodes = self.inodes.lock().unwrap();
        if path.is_empty() { return Err("enoent"); }
        let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if components.is_empty() {
            return Ok(PathLookup { ino: 0, parent_ino: 0, name: String::new() });
        }
        let mut current_ino = 0usize;
        let mut parent_ino = 0usize;
        for (i, name) in components.iter().enumerate() {
            let inode = inodes.get(&current_ino).ok_or("enoent")?;
            if inode.inode_type != InodeType::Directory { return Err("enotdir"); }
            let children = inode.children.lock().unwrap();
            if i == components.len() - 1 {
                if let Some(&ino) = children.get(*name) {
                    return Ok(PathLookup { ino, parent_ino: current_ino, name: name.to_string() });
                } else {
                    return Ok(PathLookup { ino: usize::MAX, parent_ino: current_ino, name: name.to_string() });
                }
            } else {
                if let Some(&ino) = children.get(*name) {
                    parent_ino = current_ino;
                    current_ino = ino;
                } else { return Err("enoent"); }
            }
        }
        Ok(PathLookup { ino: current_ino, parent_ino, name: String::new() })
    }

    /// 在指定目录下创建文件，返回新 inode 编号。
    pub fn create_file(&self, dir_ino: usize, name: &str) -> Result<usize, &'static str> {
        let ino = self.alloc_ino();
        let inodes = self.inodes.lock().unwrap();
        let dir = inodes.get(&dir_ino).ok_or("enoent")?;
        if dir.inode_type != InodeType::Directory { return Err("enotdir"); }
        dir.children.lock().unwrap().insert(name.to_string(), ino);
        drop(dir); drop(inodes);
        self.inodes.lock().unwrap().insert(ino, Inode::new_file(ino));
        Ok(ino)
    }

    /// 在指定目录下创建子目录。
    pub fn create_dir(&self, dir_ino: usize, name: &str) -> Result<usize, &'static str> {
        let ino = self.alloc_ino();
        let inodes = self.inodes.lock().unwrap();
        let dir = inodes.get(&dir_ino).ok_or("enoent")?;
        if dir.inode_type != InodeType::Directory { return Err("enotdir"); }
        if dir.children.lock().unwrap().contains_key(name) { return Err("eexist"); }
        dir.children.lock().unwrap().insert(name.to_string(), ino);
        drop(dir); drop(inodes);
        self.inodes.lock().unwrap().insert(ino, Inode::new_dir(ino));
        Ok(ino)
    }

    /// 删除文件（unlink）。
    pub fn unlink(&self, dir_ino: usize, name: &str) -> Result<(), &'static str> {
        let inodes = self.inodes.lock().unwrap();
        let dir = inodes.get(&dir_ino).ok_or("enoent")?;
        if dir.inode_type != InodeType::Directory { return Err("enotdir"); }
        let target_ino = dir.children.lock().unwrap().remove(name).ok_or("enoent")?;
        drop(dir); drop(inodes);
        self.inodes.lock().unwrap().remove(&target_ino);
        Ok(())
    }

    /// 确保文件有足够的块来覆盖 `block_idx`。
    fn ensure_blocks(&self, ino: usize, block_idx: usize) -> Result<(), &'static str> {
        let inodes = self.inodes.lock().unwrap();
        let inode = inodes.get(&ino).ok_or("enoent")?;
        let mut blocks = inode.blocks.lock().unwrap();
        while blocks.len() <= block_idx {
            blocks.push(self.alloc_block());
        }
        Ok(())
    }

    /// 读文件数据，走 BlockCache → Disk。
    ///
    /// 返回读到的数据。`max_len` 限制最大读取字节数。
    pub fn read_data(&self, ino: usize, offset: usize, max_len: usize, cache: &BlockCache, disk: &Disk) -> Result<Vec<u8>, &'static str> {
        let inodes = self.inodes.lock().unwrap();
        let inode = inodes.get(&ino).ok_or("enoent")?;
        if inode.inode_type != InodeType::File { return Err("eisdir"); }
        let size = *inode.size.lock().unwrap();
        if offset >= size { return Ok(Vec::new()); }
        let n = std::cmp::min(max_len, size - offset);

        let blocks = inode.blocks.lock().unwrap();
        let mut result = Vec::with_capacity(n);
        let start_blk = offset / BLOCK_SZ;
        let end_blk = (offset + n - 1) / BLOCK_SZ;

        for bi in start_blk..=end_blk {
            if bi >= blocks.len() { break; }
            let blk_no = blocks[bi];
            let blk_data = cache.fetch(blk_no, Duration::from_millis(0), disk).ok_or("io_error")?;
            let blk_off = if bi == start_blk { offset % BLOCK_SZ } else { 0 };
            let blk_end = if bi == end_blk { (offset + n - 1) % BLOCK_SZ + 1 } else { BLOCK_SZ };
            result.extend_from_slice(&blk_data[blk_off..blk_end]);
        }
        Ok(result)
    }

    /// 写文件数据，走 BlockCache → Disk。
    ///
    /// 文件不够大时自动分配新磁盘块。返回实际写入的字节数。
    pub fn write_data(&self, ino: usize, offset: usize, buf: &[u8], cache: &BlockCache, disk: &Disk) -> Result<usize, &'static str> {
        let inodes = self.inodes.lock().unwrap();
        let inode = inodes.get(&ino).ok_or("enoent")?;
        if inode.inode_type != InodeType::File { return Err("eisdir"); }
        let needed = offset + buf.len();
        let start_blk = offset / BLOCK_SZ;
        let end_blk = (needed.saturating_sub(1)) / BLOCK_SZ;

        // 自动分配不足的块
        let mut blocks = inode.blocks.lock().unwrap();
        while blocks.len() <= end_blk {
            blocks.push(self.alloc_block());
        }
        let block_list: Vec<usize> = (start_blk..=end_blk).map(|bi| blocks[bi]).collect();
        drop(blocks);

        let mut total = 0usize;
        for bi in start_blk..=end_blk {
            let blk_no = block_list[bi - start_blk];
            let mut blk_data = cache.fetch(blk_no, Duration::from_millis(0), disk).ok_or("io_error")?;
            let blk_off = if bi == start_blk { offset % BLOCK_SZ } else { 0 };
            let blk_end = if bi == end_blk { (needed.saturating_sub(1)) % BLOCK_SZ + 1 } else { BLOCK_SZ };
            let copy_len = blk_end - blk_off;
            blk_data[blk_off..blk_off + copy_len].copy_from_slice(&buf[total..total + copy_len]);
            cache.put(blk_no, blk_data);  // 更新缓存并标脏，由 tick/sync 负责写回磁盘
            total += copy_len;
        }

        // 更新文件大小
        let mut size = inode.size.lock().unwrap();
        if needed > *size { *size = needed; }

        Ok(buf.len())
    }

    /// 截断文件到指定大小，丢弃尾部多余的磁盘块。
    pub fn truncate(&self, ino: usize, size: usize) -> Result<(), &'static str> {
        let inodes = self.inodes.lock().unwrap();
        let inode = inodes.get(&ino).ok_or("enoent")?;
        let mut blocks = inode.blocks.lock().unwrap();
        let needed = (size + BLOCK_SZ - 1) / BLOCK_SZ;
        blocks.truncate(needed);
        *inode.size.lock().unwrap() = size;
        Ok(())
    }

}
