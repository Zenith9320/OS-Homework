//! ELF 可执行文件加载支持模块：提供 ELF 头部验证功能，
//! 用于检查 ELF 文件的合法性（魔数、架构、字节序等）并提取入口地址。

/// 验证 ELF 文件头部，检查其格式是否合法。
///
/// 检查项包括：
/// - 最小长度（64 字节）
/// - ELF 魔数（0x7f 'E' 'L' 'F'）
/// - 64 位格式（EI_CLASS = 2）
/// - 小端字节序（EI_DATA = 1）
/// - ELF 版本号
/// - 可执行文件类型（ET_EXEC 或 ET_DYN）
/// - 程序头是否在文件范围内
/// - 至少有一个 LOAD 类型的程序头
///
/// `data`：ELF 文件的原始字节切片。
/// 返回 `Ok(entry)` 包含入口地址，或 `Err(描述)` 表示验证失败。
pub fn validate_elf_header(data: &[u8]) -> Result<usize, &'static str> {
    eprintln!("[DBG] validate_elf_header");
    if data.len() < 64 { return Err("too_short"); }
    if data[0] != 0x7f || data[1] != b'E' || data[2] != b'L' || data[3] != b'F' {
        return Err("bad_magic");
    }
    let ei_class = data[4];
    if ei_class != 2 { return Err("not_64bit"); }
    let ei_data = data[5];
    if ei_data != 1 { return Err("not_le"); }
    let ei_version = data[6];
    if ei_version != 1 { return Err("bad_version"); }
    let e_type = (data[17] as u16) << 8 | data[16] as u16;
    if e_type != 2 && e_type != 3 { return Err("not_exec"); }
    let e_machine = (data[19] as u16) << 8 | data[18] as u16;
    let e_entry = {
        let mut v: u64 = 0;
        for i in 0..8 {
            v |= (data[24 + i] as u64) << (i * 8);
        }
        v as usize
    };
    let e_phoff = {
        let mut v: u64 = 0;
        for i in 0..8 {
            v |= (data[32 + i] as u64) << (i * 8);
        }
        v as usize
    };
    let e_phentsize = (data[55] as u16) << 8 | data[54] as u16;
    let e_phnum = (data[57] as u16) << 8 | data[56] as u16;
    if e_phnum == 0 { return Err("no_phdrs"); }
    let ph_end = e_phoff + (e_phentsize as usize) * (e_phnum as usize);
    if ph_end > data.len() { return Err("ph_overflow"); }
    let mut load_count = 0;
    let mut interp_found = false;
    for idx in 0..e_phnum as usize {
        let base = e_phoff + idx * e_phentsize as usize;
        if base + 4 > data.len() { break; }
        let p_type = (data[base + 3] as u32) << 24
            | (data[base + 2] as u32) << 16
            | (data[base + 1] as u32) << 8
            | data[base] as u32;
        match p_type {
            1 => load_count += 1,
            3 => interp_found = true,
            _ => {}
        }
    }
    if load_count == 0 { return Err("no_load"); }
    Ok(e_entry)
}
