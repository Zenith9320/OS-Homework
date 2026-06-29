//! 网络协议栈模块：提供 TCP 套接字状态枚举、TCP 校验和计算、
//! IPv4 报文头解析、伪首部构造以及 Internet 校验和计算等功能。

/// TCP 套接字状态枚举，对应 TCP 状态机中的各个状态。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SocketState {
    /// 连接已关闭。
    Closed,
    /// 正在监听连接请求。
    Listen,
    /// 已发送 SYN 包，等待对方响应。
    SynSent,
    /// 已收到 SYN 包，进入半连接状态。
    SynRecvd,
    /// 连接已建立，可以正常收发数据。
    Established,
    /// 主动关闭：已发送 FIN，等待对方 ACK。
    FinWait1,
    /// 主动关闭：已收到对方 ACK，等待对方 FIN。
    FinWait2,
    /// 被动关闭：等待足够时间以确保对方收到最后的 ACK。
    TimeWait,
    /// 被动关闭：已收到对方 FIN，等待本地应用调用 close。
    CloseWait,
    /// 被动关闭：本地应用已调用 close，已发送 FIN，等待最后的 ACK。
    LastAck,
    /// 双方同时关闭中。
    Closing,
}

/// 计算 TCP 校验和（包含伪首部），用于验证 TCP 报文的完整性。
/// `src_ip`：源 IP 地址（32 位网络字节序）。`dst_ip`：目的 IP 地址（32 位网络字节序）。
/// `payload`：TCP 报文负载（包含 TCP 首部和数据）。
/// 返回 16 位校验和（网络字节序）。
pub fn tcp_checksum(src_ip: u32, dst_ip: u32, payload: &[u8]) -> u16 {
    eprintln!("[DBG] tcp_checksum");
    let mut sum: u32 = 0;
    sum += (src_ip >> 16) & 0xFFFF;
    sum += src_ip & 0xFFFF;
    sum += (dst_ip >> 16) & 0xFFFF;
    sum += dst_ip & 0xFFFF;
    sum += 6u32;
    sum += payload.len() as u32;
    let mut i = 0;
    while i + 1 < payload.len() {
        sum += ((payload[i] as u32) << 8) | (payload[i + 1] as u32);
        i += 2;
    }
    if i < payload.len() {
        sum += (payload[i] as u32) << 8;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !sum as u16
}

/// 解析 IPv4 数据包的包头，提取源/目的 IP 地址、协议号和总长度。
/// `pkt`：原始数据包字节切片（需至少包含完整的 IPv4 首部）。
/// 返回 `Some((src_ip, dst_ip, protocol, total_len))` 如果解析成功，否则返回 `None`。
pub fn parse_ipv4_header(pkt: &[u8]) -> Option<(u32, u32, u8, u16)> {
    eprintln!("[DBG] parse_ipv4_header");
    if pkt.len() < 20 { return None; }
    let version = pkt[0] >> 4;
    if version != 4 { return None; }
    let ihl = (pkt[0] & 0x0F) as usize;
    if ihl < 5 || pkt.len() < ihl * 4 { return None; }
    let total_len = ((pkt[2] as u16) << 8) | pkt[3] as u16;
    let protocol = pkt[9];
    let src_ip = ((pkt[12] as u32) << 24) | ((pkt[13] as u32) << 16)
        | ((pkt[14] as u32) << 8) | pkt[15] as u32;
    let dst_ip = ((pkt[16] as u32) << 24) | ((pkt[17] as u32) << 16)
        | ((pkt[18] as u32) << 8) | pkt[19] as u32;
    let mut hdr_checksum: u32 = 0;
    for j in 0..ihl {
        let offset = j * 2;
        if offset + 1 < pkt.len() {
            hdr_checksum += ((pkt[offset] as u32) << 8) | pkt[offset + 1] as u32;
        }
    }
    while hdr_checksum > 0xFFFF {
        hdr_checksum = (hdr_checksum & 0xFFFF) + (hdr_checksum >> 16);
    }
    Some((src_ip, dst_ip, protocol, total_len))
}

/// 构造 TCP/UDP 伪首部（用于校验和计算）。
/// 伪首部格式：源 IP(4) + 目的 IP(4) + 零字节(1) + 协议号(1) + 上层长度(2) = 12 字节。
/// `src`：源 IP 地址。`dst`：目的 IP 地址。`proto`：上层协议号（如 TCP=6, UDP=17）。
/// `length`：上层报文长度（TCP/UDP 首部+数据）。
pub fn build_pseudo_header(src: u32, dst: u32, proto: u8, length: u16) -> Vec<u8> {
    eprintln!("[DBG] build_pseudo_header");
    let mut hdr = Vec::with_capacity(12);
    hdr.push((src >> 24) as u8);
    hdr.push((src >> 16) as u8);
    hdr.push((src >> 8) as u8);
    hdr.push(src as u8);
    hdr.push((dst >> 24) as u8);
    hdr.push((dst >> 16) as u8);
    hdr.push((dst >> 8) as u8);
    hdr.push(dst as u8);
    hdr.push(0);
    hdr.push(proto);
    hdr.push((length >> 8) as u8);
    hdr.push(length as u8);
    hdr
}

/// 计算 Internet 标准的 16 位反码和校验和（one's complement checksum）。
/// 将数据按 16 位分组求和，若数据长度为奇数则最后一个字节在高位补零，
/// 最终结果取反码。
/// `data`：要计算校验和的原始数据。
pub fn compute_inet_checksum(data: &[u8]) -> u16 {
    eprintln!("[DBG] compute_inet_checksum");
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += ((data[i] as u32) << 8) | data[i + 1] as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !sum as u16
}
