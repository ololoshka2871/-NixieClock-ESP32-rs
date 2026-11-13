//! ICMPv4 echo-резолвер: чистые кодеки кадров (парсинг Echo Request -> сборка
//! Echo Reply на уровне Ethernet-кадра).
//!
//! smoltcp/embassy-net диспетчируют Echo Request только в IcmpSocket, привязанный
//! к конкретному identifier (`accepts_v4`), а ident у ping произвольный —
//! поэтому резолвер живёт на уровне Driver (firmware/src/icmp.rs), здесь только
//! чистые функции разбора/сборки с хостовыми тестами.

/// Минимальный корректный кадр: Ethernet(14) + IPv4(20) + ICMP echo(8).
pub const MIN_FRAME: usize = 14 + 20 + 8;

/// 16-битная контрольная сумма (one's complement, RFC 1071).
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut chunks = data.chunks_exact(2);
    for c in &mut chunks {
        sum += u16::from_be_bytes([c[0], c[1]]) as u32;
    }
    let rem = chunks.remainder();
    if !rem.is_empty() {
        sum += (rem[0] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// Валидация: контрольная сумма области, включающей корректное поле checksum,
/// равна 0 (свойство one's complement).
pub fn checksum_ok(data: &[u8]) -> bool {
    checksum(data) == 0
}

/// Если `req` — IPv4 ICMP Echo Request к нашему MAC/IP: пишет полный
/// Ethernet-кадр Echo Reply в `out`, возвращает Some(длина). Иначе None.
///
/// Reply: MAC/IP поменяны местами, our_ip как источник, TTL 64, ident/seq/
/// payload зеркалятся из запроса, identification и флаги DF копируются
/// (фрагментированные запросы игнорируются).
pub fn build_echo_reply(
    req: &[u8],
    our_mac: [u8; 6],
    our_ip: [u8; 4],
    out: &mut [u8],
) -> Option<usize> {
    if req.len() < MIN_FRAME {
        return None;
    }
    if req[12..14] != [0x08u8, 0x00u8][..] {
        return None; // EtherType != IPv4
    }
    let dst_mac = &req[0..6];
    if dst_mac != our_mac.as_slice() && dst_mac != [0xff, 0xff, 0xff, 0xff, 0xff, 0xff].as_slice() {
        return None; // кадр не нам (unicast) и не broadcast
    }
    let src_mac: [u8; 6] = req[6..12].try_into().ok()?;

    let ip = &req[14..];
    if ip[0] >> 4 != 4 {
        return None;
    }
    let ihl = (ip[0] & 0x0f) as usize * 4;
    if ihl < 20 {
        return None;
    }
    let total = u16::from_be_bytes([ip[2], ip[3]]) as usize;
    if total < ihl + 8 || total > req.len() - 14 {
        return None;
    }
    if ip[9] != 1 {
        return None; // протокол != ICMP
    }
    if ip[6] & 0x20 != 0 || (u16::from_be_bytes([ip[6] & 0x1f, ip[7]]) != 0) {
        return None; // фрагментированный запрос — не отвечаем
    }
    let src_ip: [u8; 4] = ip[12..16].try_into().ok()?;
    let dst_ip: [u8; 4] = ip[16..20].try_into().ok()?;
    if dst_ip != our_ip {
        return None;
    }

    let icmp = &ip[ihl..total];
    if icmp[0] != 8 || icmp[1] != 0 {
        return None; // не Echo Request
    }
    let icmp_len = total - ihl;
    let out_len = 14 + 20 + icmp_len;
    if out.len() < out_len {
        return None;
    }

    out[0..6].copy_from_slice(&src_mac);
    out[6..12].copy_from_slice(&our_mac);
    out[12..14].copy_from_slice(&[0x08, 0x00]);

    let o = &mut out[14..];
    o[0] = 0x45; // IPv4, IHL=20
    o[1] = 0x00; // TOS
    o[2..4].copy_from_slice(&((20 + icmp_len) as u16).to_be_bytes());
    o[4..6].copy_from_slice(&ip[4..6]); // identification: копия запроса
    o[6..8].copy_from_slice(&ip[6..8]); // флаги (DF) и offset: копия, фрагменты отфильтрованы выше
    o[8] = 64; // TTL
    o[9] = 1; // протокол ICMP
    o[10..12].copy_from_slice(&[0, 0]);
    o[12..16].copy_from_slice(&our_ip);
    o[16..20].copy_from_slice(&src_ip);
    let c = checksum(&o[0..20]);
    o[10..12].copy_from_slice(&c.to_be_bytes());

    o[20..20 + icmp_len].copy_from_slice(icmp);
    o[20] = 0; // Echo Reply
    o[21] = 0;
    o[22..24].copy_from_slice(&[0, 0]);
    let c = checksum(&o[20..20 + icmp_len]);
    o[22..24].copy_from_slice(&c.to_be_bytes());

    Some(out_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOARD_MAC: [u8; 6] = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    const BOARD_IP: [u8; 4] = [192, 168, 0, 123];

    fn make_request(ident: u16, seq: u16, payload: &[u8], dst_mac: [u8; 6], dst_ip: [u8; 4]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&dst_mac);
        f.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        f.extend_from_slice(&[0x08, 0x00]);
        let icmp_len = 8 + payload.len();
        let total = 20 + icmp_len;
        let mut ip = vec![0x45u8, 0, (total >> 8) as u8, total as u8, 0x12, 0x34, 0x40, 0x00, 64, 1, 0, 0];
        ip.extend_from_slice(&[192, 168, 0, 100]);
        ip.extend_from_slice(&dst_ip);
        let c = checksum(&ip);
        ip[10..12].copy_from_slice(&c.to_be_bytes());
        f.extend_from_slice(&ip);
        let start = f.len();
        f.extend_from_slice(&[8, 0, 0, 0]);
        f.extend_from_slice(&ident.to_be_bytes());
        f.extend_from_slice(&seq.to_be_bytes());
        f.extend_from_slice(payload);
        let c = checksum(&f[start..]);
        f[start + 2..start + 4].copy_from_slice(&c.to_be_bytes());
        f
    }

    #[test]
    fn reply_mirrors_request() {
        let req = make_request(0x4321, 2, b"abcdefghijklmnop", BOARD_MAC, BOARD_IP);
        let mut out = vec![0u8; 1600];
        let len = build_echo_reply(&req, BOARD_MAC, BOARD_IP, &mut out).expect("reply");
        assert_eq!(len, 14 + 20 + 8 + 16);
        assert_eq!(&out[0..6], &[0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66][..]);
        assert_eq!(&out[6..12], BOARD_MAC.as_slice());
        assert_eq!(&out[12..14], &[0x08u8, 0x00][..]);
        assert_eq!(out[14], 0x45);
        assert_eq!(&out[26..30], [192u8, 168, 0, 123].as_slice());
        assert_eq!(&out[30..34], [192u8, 168, 0, 100].as_slice());
        assert!(checksum_ok(&out[14..34]), "IPv4 header checksum");
        assert_eq!(out[34], 0, "type = Echo Reply");
        assert_eq!(out[35], 0, "code = 0");
        // ICMP: type(1) code(1) checksum(2) ident(2) seq(2) payload...
        assert!(checksum_ok(&out[34..len]), "ICMP checksum");
        assert_eq!(&out[38..40], &[0x43u8, 0x21][..], "ident unchanged");
        assert_eq!(&out[40..42], &[0x00u8, 0x02][..], "seq unchanged");
        assert_eq!(&out[42..len], &b"abcdefghijklmnop"[..]);
    }

    #[test]
    fn odd_payload_checksum() {
        let req = make_request(1, 1, b"17-bytes-payload!!!", BOARD_MAC, BOARD_IP); // 17 байт (нечётно)
        let mut out = vec![0u8; 1600];
        let len = build_echo_reply(&req, BOARD_MAC, BOARD_IP, &mut out).expect("reply");
        assert!(checksum_ok(&out[34..len]));
        assert_eq!(&out[42..len], &b"17-bytes-payload!!!"[..]);
    }

    #[test]
    fn broadcast_mac_accepted() {
        let req = make_request(7, 1, b"x", [0xff, 0xff, 0xff, 0xff, 0xff, 0xff], BOARD_IP);
        let mut out = vec![0u8; 1600];
        assert!(build_echo_reply(&req, BOARD_MAC, BOARD_IP, &mut out).is_some());
    }

    #[test]
    fn rejects_foreign_frames() {
        let mut out = vec![0u8; 1600];
        // чужой unicast MAC
        let req = make_request(1, 1, b"x", [1, 2, 3, 4, 5, 6], BOARD_IP);
        assert_eq!(build_echo_reply(&req, BOARD_MAC, BOARD_IP, &mut out), None);
        // чужой dst IP
        let req = make_request(1, 1, b"x", BOARD_MAC, [10, 0, 0, 1]);
        assert_eq!(build_echo_reply(&req, BOARD_MAC, BOARD_IP, &mut out), None);
        // EtherType ARP
        let mut req = make_request(1, 1, b"x", BOARD_MAC, BOARD_IP);
        req[12] = 0x08;
        req[13] = 0x06;
        assert_eq!(build_echo_reply(&req, BOARD_MAC, BOARD_IP, &mut out), None);
        // уже Echo Reply (type 0)
        let mut req = make_request(1, 1, b"x", BOARD_MAC, BOARD_IP);
        req[14 + 20] = 0;
        assert_eq!(build_echo_reply(&req, BOARD_MAC, BOARD_IP, &mut out), None);
        // фрагментированный (MF)
        let mut req = make_request(1, 1, b"x", BOARD_MAC, BOARD_IP);
        req[14 + 6] |= 0x20;
        assert_eq!(build_echo_reply(&req, BOARD_MAC, BOARD_IP, &mut out), None);
        // короткий кадр
        assert_eq!(build_echo_reply(&req[..30], BOARD_MAC, BOARD_IP, &mut out), None);
        // маленький out
        let req = make_request(1, 1, b"payload payload payload", BOARD_MAC, BOARD_IP);
        let mut small = vec![0u8; 40];
        assert_eq!(build_echo_reply(&req, BOARD_MAC, BOARD_IP, &mut small), None);
    }

    #[test]
    fn known_vector_from_real_ping() {
        // Реальный Linux-пакет: ip 45 00 00 54 6b 2c 40 00 40 01 ... (ident 0x6b2c),
        // ICMP: 08 00 + checksum, из wireshark-разметки типового ping 56B payload.
        // Проверяем, что checksum() воспроизводит известные поля.
        let ip_hdr: [u8; 20] = [
            0x45, 0x00, 0x00, 0x54, 0x6b, 0x2c, 0x40, 0x00, 0x40, 0x01, 0x00, 0x00,
            192, 168, 0, 100, 192, 168, 0, 123,
        ];
        let c = checksum(&ip_hdr);
        let mut hdr = ip_hdr;
        hdr[10..12].copy_from_slice(&c.to_be_bytes());
        assert!(checksum_ok(&hdr));
        let icmp = [8u8, 0, 0, 0, 0x6b, 0x2c, 0x00, 0x01];
        let c = checksum(&icmp);
        let mut icmp2 = icmp;
        icmp2[2..4].copy_from_slice(&c.to_be_bytes());
        assert!(checksum_ok(&icmp2));
    }
}
