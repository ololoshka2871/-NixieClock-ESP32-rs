//! Минимальный DHCPv4-кодек сервера (RFC 2131/2132) для SoftAP ровно с одним
//! клиентом: DISCOVER -> OFFER -> REQUEST -> ACK, фиксированный адрес клиента
//! 192.168.139.100, AP 192.168.139.1/24. Чистые функции: no_std, heapless,
//! без alloc и UDP-обвязки — порт/вещание (dst :68, broadcast/unicast) решает
//! транспортный слой firmware. Ошибки — статические &'static str.
//!
//! Смещения полей BOOTP-заголовка, magic cookie и TLV-опции записаны вручную
//! и закреплены побайтовыми хост-тестами (`cargo test -p clock-core`).

use heapless::Vec;

// ----- Фиксированная сеть SoftAP ---------------------------------------------
pub const AP_IP: [u8; 4] = [192, 168, 139, 1];
pub const CLIENT_IP: [u8; 4] = [192, 168, 139, 100];
pub const SUBNET_MASK: [u8; 4] = [255, 255, 255, 0];
/// Lease OFFER/ACK (option 51), сек.
pub const LEASE_SECS: u32 = 3600;
/// Карантин клиента после DECLINE (защита от флипка), сек.
pub const DECLINE_COOLDOWN_SECS: u64 = 60;

// ----- Заголовок BOOTP (RFC 2131 section 2): смещения, размер в комментарии --
pub const OP_REQUEST: u8 = 1; // op@0 (1 B): запрос клиента
pub const OP_REPLY: u8 = 2; // op@0 (1 B): ответ сервера
pub const HTYPE_ETHER: u8 = 1; // htype@1 (1 B): Ethernet
pub const HLEN_ETHER: u8 = 6; // hlen@2 (1 B): длина MAC
pub const OFF_XID: usize = 4; // xid (4 B, u32 BE)   4..8
pub const OFF_SECS: usize = 8; // secs (2 B, BE)     8..10
pub const OFF_FLAGS: usize = 10; // flags (2 B, BE)  10..12, bit15 = broadcast
pub const OFF_CIADDR: usize = 12; // ciaddr (4 B)    12..16 (IP клиента)
pub const OFF_YIADDR: usize = 16; // yiaddr (4 B)    16..20 («ваш» IP)
pub const OFF_SIADDR: usize = 20; // siaddr (4 B)    20..24 (адрес сервера)
pub const OFF_GIADDR: usize = 24; // giaddr (4 B)    24..28 (relay-агент)
pub const OFF_CHADDR: usize = 28; // chaddr (16 B)   28..44, первые 6 — MAC
// BOOTP-заголовок = 44 + sname(64) + file(128) = 236, НЕ 240.
// 240 — уже первая опция после 4-байтового magic cookie.
pub const HEADER_LEN: usize = 236; // sname (64 B) 44..108, file (128 B) 108..236
pub const OFF_COOKIE: usize = 236; // magic cookie (4 B) 236..240
pub const OFF_OPTS: usize = 240; // начало TLV-опций
pub const MAGIC_COOKIE: [u8; 4] = [0x63, 0x82, 0x53, 0x63];
pub const FLAG_BROADCAST: u16 = 0x8000;

// ----- Типы сообщений DHCP: значения option 53 (RFC 2131 section 9.6) -------
pub const DHCP_DISCOVER: u8 = 1;
pub const DHCP_OFFER: u8 = 2;
pub const DHCP_REQUEST: u8 = 3;
pub const DHCP_DECLINE: u8 = 4;
pub const DHCP_ACK: u8 = 5;
pub const DHCP_NAK: u8 = 6;
pub const DHCP_RELEASE: u8 = 7;
pub const DHCP_INFORM: u8 = 8;

// ----- Коды опций (RFC 2132 section 3) ---------------------------------------
pub const OPT_PAD: u8 = 0; // без байта длины
pub const OPT_SUBNET_MASK: u8 = 1; // len 4
pub const OPT_ROUTER: u8 = 3; // len 4 (шлюз = AP_IP)
pub const OPT_DNS: u8 = 6; // len кратно 4; в наш payload не кладётся
pub const OPT_REQUESTED_IP: u8 = 50; // len 4 (клиент в REQUEST)
pub const OPT_LEASE_TIME: u8 = 51; // len 4, u32 BE, секунды
pub const OPT_MSG_TYPE: u8 = 53; // len 1
pub const OPT_SERVER_ID: u8 = 54; // len 4 (адрес сервера)
pub const OPT_END: u8 = 255; // без байта длины

/// Полный OFFER/ACK: 236 + cookie(4) + 53(3) + 54(6) + 51(6) + 1(6) + 3(6)
/// + END(1) = 268 байт (TLV = код+длина+значение, опции len 4 занимают 6);
/// NAK короче: 240 + 3 + 6 + 1 = 250. Дейтаграмма DHCP бывает до 576 B —
/// буфер с запасом.
pub const MAX_REPLY: usize = 276;

const E_SHORT: &str = "dhcp: короче заголовка+cookie (240 B)";
const E_OP: &str = "dhcp: op не 1 и не 2";
const E_HTYPE: &str = "dhcp: htype/hlen не 1/6 (Ethernet)";
const E_COOKIE: &str = "dhcp: нет magic cookie 63 82 53 63";
const E_OPTS: &str = "dhcp: TLV-опции обрезаны или нет маркера END";
const E_MSG_TYPE: &str = "dhcp: нет option 53 (тип сообщения, len 1)";
const E_NOT_DISCOVER: &str = "dhcp: не DISCOVER (option 53 != 1)";
const E_NOT_REQUEST: &str = "dhcp: не REQUEST (option 53 != 3)";
const E_BUF: &str = "dhcp: буфер ответа переполнен";

/// Разобранное клиентское (или наше) DHCP-сообщение.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub xid: u32,
    pub secs: u16,
    pub broadcast: bool,
    pub chaddr: [u8; 6],
    pub ciaddr: [u8; 4],
    pub msg_type: u8,
    /// option 50, если клиент её прислал (SELECTING / REQUEST с адресом).
    pub requested_ip: Option<[u8; 4]>,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn ip_at(b: &[u8], o: usize) -> [u8; 4] {
    [b[o], b[o + 1], b[o + 2], b[o + 3]]
}

/// Обход TLV: код(1) длина(1) значение(len). PAD(0) пропускается, END(255)
/// завершает. Неизвестные опции (55, 61, ...) молча игнорируются.
fn parse_options(b: &[u8], r: &mut Request) -> Result<(), &'static str> {
    let mut i = OFF_OPTS;
    loop {
        if i >= b.len() {
            return Err(E_OPTS);
        }
        let code = b[i];
        if code == OPT_PAD {
            i += 1;
            continue;
        }
        if code == OPT_END {
            return Ok(());
        }
        if i + 2 > b.len() {
            return Err(E_OPTS);
        }
        let len = b[i + 1] as usize;
        // Нулевая длина у неизвестной опции — норма (Android, например, шлёт
        // пустые option 12/55). Такие опции просто игнорируем, а не бракую
        // весь пакет: раньше из-за этого три первых DHCP-пакета телефона
        // уходили в WARN "TLV-опции обрезаны".
        if i + 2 + len > b.len() {
            return Err(E_OPTS);
        }
        let v = &b[i + 2..i + 2 + len];
        match code {
            OPT_MSG_TYPE if len == 1 => r.msg_type = v[0],
            OPT_REQUESTED_IP if len == 4 => r.requested_ip = Some([v[0], v[1], v[2], v[3]]),
            _ => {}
        }
        i += 2 + len;
    }
}

/// Полный разбор запроса или ответа (op 1/2): заголовок + cookie + TLV.
pub fn parse(b: &[u8]) -> Result<Request, &'static str> {
    if b.len() < OFF_OPTS {
        return Err(E_SHORT);
    }
    if b[0] != OP_REQUEST && b[0] != OP_REPLY {
        return Err(E_OP);
    }
    if b[1] != HTYPE_ETHER || b[2] != HLEN_ETHER {
        return Err(E_HTYPE);
    }
    if b[OFF_COOKIE..OFF_COOKIE + 4] != MAGIC_COOKIE[..] {
        return Err(E_COOKIE);
    }
    let mut r = Request {
        xid: u32_at(b, OFF_XID),
        secs: u16_at(b, OFF_SECS),
        broadcast: u16_at(b, OFF_FLAGS) & FLAG_BROADCAST != 0,
        chaddr: [
            b[OFF_CHADDR],
            b[OFF_CHADDR + 1],
            b[OFF_CHADDR + 2],
            b[OFF_CHADDR + 3],
            b[OFF_CHADDR + 4],
            b[OFF_CHADDR + 5],
        ],
        ciaddr: ip_at(b, OFF_CIADDR),
        msg_type: 0,
        requested_ip: None,
    };
    parse_options(b, &mut r)?;
    if r.msg_type == 0 {
        return Err(E_MSG_TYPE);
    }
    Ok(r)
}

pub fn parse_discover(b: &[u8]) -> Result<Request, &'static str> {
    let r = parse(b)?;
    if r.msg_type != DHCP_DISCOVER {
        return Err(E_NOT_DISCOVER);
    }
    Ok(r)
}

pub fn parse_request(b: &[u8]) -> Result<Request, &'static str> {
    let r = parse(b)?;
    if r.msg_type != DHCP_REQUEST {
        return Err(E_NOT_REQUEST);
    }
    Ok(r)
}

// ----- Сборка ответов ---------------------------------------------------------

/// Каркас BOOTREPLY: xid и chaddr зеркалятся из запроса, yiaddr/siaddr
/// заполняются, остальное (hops, secs, flags, ciaddr, giaddr, sname, file) — 0.
fn new_reply(req: &Request, server_ip: [u8; 4], yiaddr: [u8; 4]) -> Result<Vec<u8, MAX_REPLY>, &'static str> {
    let mut b: Vec<u8, MAX_REPLY> = Vec::new();
    // нуль-заполняем заголовок и слот cookie (индексация Vec ограничена len,
    // поэтому cookie пишется только после расширения до OFF_OPTS = 240)
    while b.len() < OFF_OPTS {
        b.push(0u8).map_err(|_| E_BUF)?;
    }
    b[0] = OP_REPLY;
    b[1] = HTYPE_ETHER;
    b[2] = HLEN_ETHER;
    b[OFF_XID..OFF_XID + 4].copy_from_slice(&req.xid.to_be_bytes());
    b[OFF_YIADDR..OFF_YIADDR + 4].copy_from_slice(&yiaddr);
    b[OFF_SIADDR..OFF_SIADDR + 4].copy_from_slice(&server_ip);
    b[OFF_CHADDR..OFF_CHADDR + 6].copy_from_slice(&req.chaddr);
    b[OFF_COOKIE..OFF_COOKIE + 4].copy_from_slice(&MAGIC_COOKIE);
    Ok(b)
}

fn push_opt(b: &mut Vec<u8, MAX_REPLY>, code: u8, val: &[u8]) -> Result<(), &'static str> {
    b.push(code).map_err(|_| E_BUF)?;
    b.push(val.len() as u8).map_err(|_| E_BUF)?;
    for &x in val {
        b.push(x).map_err(|_| E_BUF)?;
    }
    Ok(())
}

/// Общий набор опций OFFER и ACK: 53 тип, 54 server id, 51 lease, 1 маска,
/// 3 шлюз, END. DNS (6) не кладём — redirect DNS решается отдельным слоем.
fn push_reply_opts(
    b: &mut Vec<u8, MAX_REPLY>,
    msg_type: u8,
    server_ip: [u8; 4],
    lease_secs: u32,
) -> Result<(), &'static str> {
    push_opt(b, OPT_MSG_TYPE, &[msg_type])?;
    push_opt(b, OPT_SERVER_ID, &server_ip)?;
    push_opt(b, OPT_LEASE_TIME, &lease_secs.to_be_bytes())?;
    push_opt(b, OPT_SUBNET_MASK, &SUBNET_MASK)?;
    push_opt(b, OPT_ROUTER, &server_ip)?;
    b.push(OPT_END).map_err(|_| E_BUF)?;
    Ok(())
}

pub fn build_offer(
    req: &Request,
    client_ip: [u8; 4],
    server_ip: [u8; 4],
    lease_secs: u32,
) -> Result<Vec<u8, MAX_REPLY>, &'static str> {
    let mut b = new_reply(req, server_ip, client_ip)?;
    push_reply_opts(&mut b, DHCP_OFFER, server_ip, lease_secs)?;
    Ok(b)
}

pub fn build_ack(
    req: &Request,
    client_ip: [u8; 4],
    server_ip: [u8; 4],
    lease_secs: u32,
) -> Result<Vec<u8, MAX_REPLY>, &'static str> {
    let mut b = new_reply(req, server_ip, client_ip)?;
    push_reply_opts(&mut b, DHCP_ACK, server_ip, lease_secs)?;
    Ok(b)
}

/// NAK: yiaddr = 0.0.0.0 (адрес не выдан), опции только 53/54/END.
pub fn build_nak(req: &Request, server_ip: [u8; 4]) -> Result<Vec<u8, MAX_REPLY>, &'static str> {
    let mut b = new_reply(req, server_ip, [0u8; 4])?;
    push_opt(&mut b, OPT_MSG_TYPE, &[DHCP_NAK])?;
    push_opt(&mut b, OPT_SERVER_ID, &server_ip)?;
    b.push(OPT_END).map_err(|_| E_BUF)?;
    Ok(b)
}

// ----- Таблица аренды одного клиента: IDLE -> OFFERED -> ACKED ---------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseState {
    Idle,
    Offered,
    Acked,
    Declined, // карантин после DECLINE клиента
}

/// Однослотовая аренда единственного клиента. `until` — абсолютное
/// monotonic-время (сек) конца lease или конца карантина Declined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    pub state: LeaseState,
    pub mac: [u8; 6],
    pub xid: u32,
    pub until: u64,
}

/// Действие, которое firmware обязано выполнить по результату события.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseAction {
    Offer, // send build_offer
    Ack,   // send build_ack
    Nak,   // send build_nak
    Ignore,
}

impl Lease {
    pub const fn idle() -> Self {
        Self {
            state: LeaseState::Idle,
            mac: [0u8; 6],
            xid: 0,
            until: 0,
        }
    }

    fn offer_now(&mut self, req: &Request, now: u64, lease: u32) {
        self.state = LeaseState::Offered;
        self.mac = req.chaddr;
        self.xid = req.xid;
        self.until = now.saturating_add(lease as u64);
    }

    fn ack_now(&mut self, req: &Request, now: u64, lease: u32) {
        self.state = LeaseState::Acked;
        self.mac = req.chaddr;
        self.xid = req.xid;
        self.until = now.saturating_add(lease as u64);
    }

    /// SELECTING: option 50 == CLIENT_IP. RENEW/INIT-REBOOT: ciaddr == CLIENT_IP
    /// без option 50. Всё остальное — чужой адрес.
    fn request_ok(&self, req: &Request) -> bool {
        match req.requested_ip {
            Some(ip) => ip == CLIENT_IP,
            None => req.ciaddr == CLIENT_IP,
        }
    }

    /// Событие (now, разобранный клиентский пакет) -> следующее состояние +
    /// действие. DISCOVER-шторм: повторные DISCOVER того же MAC в OFFERED
    /// игнорируются (OFFER уже отправлен). Чужой MAC при активном слоте
    /// игнорируется (клиент на SoftAP ровно один).
    pub fn on(&mut self, req: &Request, now: u64, lease: u32) -> LeaseAction {
        if self.state != LeaseState::Idle && now >= self.until {
            *self = Self::idle(); // lease или карантин истёк
        }
        match self.state {
            LeaseState::Idle => match req.msg_type {
                DHCP_DISCOVER => {
                    self.offer_now(req, now, lease);
                    LeaseAction::Offer
                }
                // Запрос без предварительного OFFER или после NAK/RELEASE.
                DHCP_REQUEST => LeaseAction::Nak,
                _ => LeaseAction::Ignore,
            },
            LeaseState::Offered => {
                if req.chaddr != self.mac {
                    return LeaseAction::Ignore;
                }
                match req.msg_type {
                    // Повторный DISCOVER — не молчим: первый OFFER мог потеряться
                    // в эфире. Перевыставляем аренду и шлём свежий OFFER с новым
                    // xid (клиент всё равно ещё не получил ACK).
                    DHCP_DISCOVER => {
                        self.offer_now(req, now, lease);
                        LeaseAction::Offer
                    }
                    DHCP_REQUEST if self.request_ok(req) => {
                        self.ack_now(req, now, lease);
                        LeaseAction::Ack
                    }
                    DHCP_REQUEST => {
                        *self = Self::idle(); // NAK: клиент начнёт с DISCOVER
                        LeaseAction::Nak
                    }
                    DHCP_DECLINE => {
                        self.state = LeaseState::Declined;
                        self.until = now.saturating_add(DECLINE_COOLDOWN_SECS);
                        LeaseAction::Ignore
                    }
                    _ => LeaseAction::Ignore,
                }
            }
            LeaseState::Acked => {
                if req.chaddr != self.mac {
                    return LeaseAction::Ignore;
                }
                match req.msg_type {
                    DHCP_REQUEST if self.request_ok(req) => {
                        self.ack_now(req, now, lease); // RENEW / SELECTING
                        LeaseAction::Ack
                    }
                    DHCP_REQUEST => {
                        *self = Self::idle();
                        LeaseAction::Nak // чужой IP: NAK + переначать DORA
                    }
                    DHCP_DISCOVER => {
                        self.offer_now(req, now, lease); // клиент перезапустил интерфейс
                        LeaseAction::Offer
                    }
                    DHCP_DECLINE => {
                        self.state = LeaseState::Declined;
                        self.until = now.saturating_add(DECLINE_COOLDOWN_SECS);
                        LeaseAction::Ignore
                    }
                    DHCP_RELEASE => {
                        *self = Self::idle();
                        LeaseAction::Ignore
                    }
                    _ => LeaseAction::Ignore,
                }
            }
            LeaseState::Declined => LeaseAction::Ignore, // ждём окончания карантина
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5];
    const MAC2: [u8; 6] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    const XID: u32 = 0xDEAD_BEEF;

    fn pushb<const N: usize>(v: &mut Vec<u8, N>, b: &[u8]) {
        for &x in b {
            v.push(x).unwrap();
        }
    }

    /// Собственный клиентский пакет по смещениям выше: вектор строится из
    /// констант модуля, а не из чужих дампов.
    fn client_msg(
        msg_type: u8,
        flags: u16,
        ciaddr: [u8; 4],
        mac: [u8; 6],
        opt50: Option<[u8; 4]>,
        extra_opts: &[u8],
    ) -> Vec<u8, 576> {
        let mut p: Vec<u8, 576> = Vec::new();
        while p.len() < OFF_OPTS {
            p.push(0u8).unwrap();
        }
        p[0] = OP_REQUEST;
        p[1] = HTYPE_ETHER;
        p[2] = HLEN_ETHER;
        p[OFF_XID..OFF_XID + 4].copy_from_slice(&XID.to_be_bytes());
        p[OFF_SECS..OFF_SECS + 2].copy_from_slice(&3u16.to_be_bytes());
        p[OFF_FLAGS..OFF_FLAGS + 2].copy_from_slice(&flags.to_be_bytes());
        p[OFF_CIADDR..OFF_CIADDR + 4].copy_from_slice(&ciaddr);
        p[OFF_CHADDR..OFF_CHADDR + 6].copy_from_slice(&mac);
        p[OFF_COOKIE..OFF_COOKIE + 4].copy_from_slice(&MAGIC_COOKIE);
        pushb(&mut p, &[OPT_MSG_TYPE, 1, msg_type]);
        if let Some(ip) = opt50 {
            pushb(&mut p, &[OPT_REQUESTED_IP, 4]);
            pushb(&mut p, &ip);
        }
        pushb(&mut p, extra_opts);
        p.push(OPT_END).unwrap();
        p
    }

    /// Разбор TLV-области ответа в пары (код, длина) до END.
    fn tlv_pairs(b: &[u8]) -> [(u8, u8); 6] {
        let mut got = [(0u8, 0u8); 6];
        let mut n = 0usize;
        let mut i = OFF_OPTS;
        loop {
            let code = b[i];
            if code == OPT_END {
                got[n] = (code, 0);
                return got;
            }
            got[n] = (code, b[i + 1]);
            n += 1;
            i += 2 + b[i + 1] as usize;
        }
    }

    // ---- Побайтовые векторы заголовка ----------------------------------------

    #[test]
    fn discover_vector_offsets_are_exact() {
        let d = client_msg(
            DHCP_DISCOVER,
            0x8000,
            [0, 0, 0, 0],
            MAC,
            None,
            &[55, 3, 1, 3, 6], // parameter request list: маска, шлюз, DNS
        );
        assert_eq!(d.len(), 249);
        assert_eq!(d[0], OP_REQUEST);
        assert_eq!(d[1], HTYPE_ETHER);
        assert_eq!(d[2], HLEN_ETHER);
        assert_eq!(d[3], 0); // hops
        assert_eq!(&d[4..8], &[0xDE, 0xAD, 0xBE, 0xEF][..]); // xid BE
        assert_eq!(&d[8..10], &[0x00, 0x03][..]); // secs = 3
        assert_eq!(&d[10..12], &[0x80, 0x00][..]); // flags: broadcast
        assert_eq!(&d[12..16], &[0, 0, 0, 0][..]); // ciaddr = 0 в DISCOVER
        assert_eq!(&d[24..28], &[0, 0, 0, 0][..]); // giaddr = 0 (нет relay)
        assert_eq!(&d[28..34], &[0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5][..]); // chaddr
        assert_eq!(&d[236..240], &[0x63, 0x82, 0x53, 0x63][..]); // magic cookie
        assert_eq!(&d[240..243], &[53, 1, 1][..]); // opt 53 DISCOVER
        assert_eq!(&d[243..248], &[55, 3, 1, 3, 6][..]);
        assert_eq!(d[248], OPT_END);
    }

    #[test]
    fn parse_discover_extracts_fields() {
        let d = client_msg(DHCP_DISCOVER, 0x8000, [0, 0, 0, 0], MAC, None, &[55, 3, 1, 3, 6]);
        let r = parse_discover(&d).unwrap();
        assert_eq!(r.xid, XID);
        assert_eq!(r.secs, 3);
        assert!(r.broadcast);
        assert_eq!(r.chaddr, MAC);
        assert_eq!(r.ciaddr, [0, 0, 0, 0]);
        assert_eq!(r.msg_type, DHCP_DISCOVER);
        assert_eq!(r.requested_ip, None);
    }

    // ---- Побайтовые ответы ----------------------------------------------------

    #[test]
    fn offer_is_byte_exact() {
        let d = client_msg(DHCP_DISCOVER, 0x8000, [0, 0, 0, 0], MAC, None, &[]);
        let r = parse(&d).unwrap();
        let o = build_offer(&r, CLIENT_IP, AP_IP, 3600).unwrap();
        assert_eq!(o.len(), 268);
        assert_eq!(o[0], OP_REPLY);
        assert_eq!(o[1], HTYPE_ETHER);
        assert_eq!(o[2], HLEN_ETHER);
        assert_eq!(&o[4..8], &[0xDE, 0xAD, 0xBE, 0xEF][..]); // xid зеркало
        assert_eq!(&o[12..16], &[0, 0, 0, 0][..]); // ciaddr в ответе = 0
        assert_eq!(&o[16..20], &[192, 168, 139, 100][..]); // yiaddr = адрес клиента
        assert_eq!(&o[20..24], &[192, 168, 139, 1][..]); // siaddr = сервер
        assert_eq!(&o[24..28], &[0, 0, 0, 0][..]); // giaddr = 0
        assert_eq!(&o[28..34], &[0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5][..]); // chaddr зеркало
        assert!(o[34..44].iter().all(|&x| x == 0)); // хвост chaddr обнулён
        assert!(o[44..236].iter().all(|&x| x == 0)); // sname + file обнулены
        assert_eq!(&o[236..240], &[0x63, 0x82, 0x53, 0x63][..]);
        assert_eq!(
            &o[240..268],
            &[
                53, 1, 2, // OFFER
                54, 4, 192, 168, 139, 1, // server id
                51, 4, 0, 0, 14, 16, // lease 3600 = 0x0E10
                1, 4, 255, 255, 255, 0, // маска
                3, 4, 192, 168, 139, 1, // шлюз
                255, // END
            ][..]
        );
        assert_eq!(
            &tlv_pairs(&o)[..6],
            &[(53, 1), (54, 4), (51, 4), (1, 4), (3, 4), (255, 0)]
        );
    }

    #[test]
    fn ack_is_byte_exact() {
        let q = client_msg(
            DHCP_REQUEST,
            0x8000,
            [0, 0, 0, 0],
            MAC,
            Some(CLIENT_IP),
            &[],
        );
        let r = parse_request(&q).unwrap();
        let a = build_ack(&r, CLIENT_IP, AP_IP, 3600).unwrap();
        assert_eq!(a.len(), 268);
        assert_eq!(&a[16..20], &[192, 168, 139, 100][..]);
        assert_eq!(
            &tlv_pairs(&a)[..6],
            &[(53, 1), (54, 4), (51, 4), (1, 4), (3, 4), (255, 0)]
        );
        assert_eq!(a[242], DHCP_ACK);
    }

    #[test]
    fn nak_is_byte_exact() {
        let q = client_msg(DHCP_REQUEST, 0, [0, 0, 0, 0], MAC, Some([10, 0, 0, 5]), &[]);
        let r = parse_request(&q).unwrap();
        let n = build_nak(&r, AP_IP).unwrap();
        assert_eq!(n.len(), 250);
        assert_eq!(&n[16..20], &[0, 0, 0, 0][..]); // NAK: адрес не выдан
        assert_eq!(&n[4..8], &[0xDE, 0xAD, 0xBE, 0xEF][..]);
        assert_eq!(&n[240..250], &[53, 1, 6, 54, 4, 192, 168, 139, 1, 255][..]);
    }

    #[test]
    fn roundtrip_parse_own_reply() {
        let d = client_msg(DHCP_DISCOVER, 0x8000, [0, 0, 0, 0], MAC, None, &[]);
        let r = parse(&d).unwrap();
        let o = build_offer(&r, CLIENT_IP, AP_IP, 3600).unwrap();
        let back = parse(&o).unwrap();
        assert_eq!(back.msg_type, DHCP_OFFER);
        assert_eq!(back.xid, XID);
        assert_eq!(back.chaddr, MAC);
        assert!(parse_discover(&o).is_err()); // OFFER не DISCOVER
    }

    #[test]
    fn broadcast_flag_parsed() {
        let u = client_msg(DHCP_DISCOVER, 0x0000, [0, 0, 0, 0], MAC, None, &[]);
        assert!(!parse(&u).unwrap().broadcast);
        let b = client_msg(DHCP_DISCOVER, FLAG_BROADCAST, [0, 0, 0, 0], MAC, None, &[]);
        assert!(parse(&b).unwrap().broadcast);
    }

    // ---- Мусор и нарушения ----------------------------------------------------

    #[test]
    fn malformed_rejects_all() {
        let d = client_msg(DHCP_DISCOVER, 0x8000, [0, 0, 0, 0], MAC, None, &[]);

        // слишком короткий
        assert_eq!(parse(&d[..100]), Err(E_SHORT));
        assert_eq!(parse(&d[..239]), Err(E_SHORT));

        // ровно заголовок+cookie, ни одной опции (нет END)
        assert_eq!(parse(&d[..240]), Err(E_OPTS));

        // испорченный cookie
        let mut bad = d.clone();
        bad[OFF_COOKIE + 1] = 0x00;
        assert_eq!(parse(&bad), Err(E_COOKIE));

        // плохой op
        let mut bad = d.clone();
        bad[0] = 3;
        assert_eq!(parse(&bad), Err(E_OP));

        // плохой hlen (не Ethernet)
        let mut bad = d.clone();
        bad[2] = 4;
        assert_eq!(parse(&bad), Err(E_HTYPE));

        // нет option 53
        let n53 = client_msg(DHCP_DISCOVER, 0, [0, 0, 0, 0], MAC, None, &[1, 4, 255, 255, 255, 0]);
        // msg_type там всё же есть... строим пакет без 53 вручную:
        let mut no53: Vec<u8, 576> = Vec::new();
        pushb(&mut no53, &n53[..OFF_OPTS]);
        pushb(&mut no53, &[1, 4, 255, 255, 255, 0, 255]);
        assert_eq!(parse(&no53), Err(E_MSG_TYPE));

        // TLV обрезан: option 51 объявляет len 4, а байт не хватает
        let mut cut: Vec<u8, 576> = Vec::new();
        pushb(&mut cut, &n53[..OFF_OPTS]);
        pushb(&mut cut, &[51, 4, 0, 0]);
        assert_eq!(parse(&cut), Err(E_OPTS));

        // option 53 с неверной длиной (4 вместо 1) — ignored -> типа нет
        let mut bad53: Vec<u8, 576> = Vec::new();
        pushb(&mut bad53, &n53[..OFF_OPTS]);
        pushb(&mut bad53, &[53, 4, 0, 0, 0, 1, 255]);
        assert_eq!(parse(&bad53), Err(E_MSG_TYPE));
    }

    #[test]
    fn pads_and_garbage_after_end_ignored() {
        let d = client_msg(DHCP_DISCOVER, 0, [0, 0, 0, 0], MAC, None, &[]);
        let mut p: Vec<u8, 576> = Vec::new();
        pushb(&mut p, &d[..OFF_OPTS]);
        pushb(&mut p, &[0, 0, 53, 1, 1, 255]); // PAD перед опциями
        pushb(&mut p, &[0xAA, 0xBB]); // мусор после END
        let r = parse(&p).unwrap();
        assert_eq!(r.msg_type, DHCP_DISCOVER);
    }

    #[test]
    fn zero_length_options_are_tolerated() {
        // Android шлёт опции с len=0 (hostname / request list). Раньше это
        // давало E_OPTS и пакет отбрасывался, хотя type=1 разбирался бы.
        let d = client_msg(DHCP_DISCOVER, 0, [0; 4], MAC, None, &[]);
        let mut p: Vec<u8, 576> = Vec::new();
        pushb(&mut p, &d[..OFF_OPTS]);
        pushb(&mut p, &[12, 0, 53, 1, 1, 55, 0, 255]);
        let r = parse(&p).unwrap();
        assert_eq!(r.msg_type, DHCP_DISCOVER);
        assert_eq!(r.chaddr, MAC);
    }

    #[test]
    fn request_captures_option_50() {
        let q = client_msg(DHCP_REQUEST, 0x8000, [0, 0, 0, 0], MAC, Some(CLIENT_IP), &[]);
        let r = parse_request(&q).unwrap();
        assert_eq!(r.requested_ip, Some(CLIENT_IP));
        let other = client_msg(DHCP_REQUEST, 0x8000, [0, 0, 0, 0], MAC, Some([10, 0, 0, 5]), &[]);
        assert_eq!(parse_request(&other).unwrap().requested_ip, Some([10, 0, 0, 5]));
    }

    // ---- Машина состояний аренды ----------------------------------------------

    fn req_of(msg_type: u8, mac: [u8; 6], ciaddr: [u8; 4], opt50: Option<[u8; 4]>) -> Request {
        let p = client_msg(msg_type, 0x8000, ciaddr, mac, opt50, &[]);
        parse(&p).unwrap()
    }

    #[test]
    fn lease_happy_path_dora() {
        let mut l = Lease::idle();
        assert_eq!(l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 0, 3600), LeaseAction::Offer);
        assert_eq!(l.state, LeaseState::Offered);
        assert_eq!(l.until, 3600);
        assert_eq!(
            l.on(&req_of(DHCP_REQUEST, MAC, [0; 4], Some(CLIENT_IP)), 2, 3600),
            LeaseAction::Ack
        );
        assert_eq!(l.state, LeaseState::Acked);
        assert_eq!(l.until, 3602);
    }

    #[test]
    fn lease_discover_storm_reoffers() {
        let mut l = Lease::idle();
        assert_eq!(l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 0, 3600), LeaseAction::Offer);
        // Первый OFFER мог не дойти до клиента: каждый повторный DISCOVER
        // обязан вызывать свежий OFFER, иначе Setup-AP остаётся без ответа.
        for t in 1..=5u64 {
            assert_eq!(l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), t, 3600), LeaseAction::Offer);
            assert_eq!(l.state, LeaseState::Offered);
        }
    }

    #[test]
    fn lease_foreign_mac_ignored() {
        let mut l = Lease::idle();
        l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 0, 3600);
        assert_eq!(l.on(&req_of(DHCP_DISCOVER, MAC2, [0; 4], None), 1, 3600), LeaseAction::Ignore);
        assert_eq!(l.on(&req_of(DHCP_REQUEST, MAC2, [0; 4], Some(CLIENT_IP)), 1, 3600), LeaseAction::Ignore);
        assert_eq!(l.mac, MAC);
    }

    #[test]
    fn lease_foreign_ip_naks_and_recovers() {
        let mut l = Lease::idle();
        assert_eq!(l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 0, 3600), LeaseAction::Offer);
        assert_eq!(
            l.on(&req_of(DHCP_REQUEST, MAC, [0; 4], Some([10, 0, 0, 5])), 2, 3600),
            LeaseAction::Nak
        );
        assert_eq!(l.state, LeaseState::Idle);
        // после NAK клиент перезапускает DORA — OFFER того же фиксированного адреса
        assert_eq!(l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 5, 3600), LeaseAction::Offer);
    }

    #[test]
    fn lease_renew_in_acked() {
        let mut l = Lease::idle();
        l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 0, 3600);
        l.on(&req_of(DHCP_REQUEST, MAC, [0; 4], Some(CLIENT_IP)), 2, 3600);
        assert_eq!(l.state, LeaseState::Acked);
        // RENEW: unicast (ciaddr = клиент, flags 0), option 50 не обязательна
        let renew = client_msg(DHCP_REQUEST, 0, CLIENT_IP, MAC, None, &[]);
        assert_eq!(l.on(&parse(&renew).unwrap(), 1800, 3600), LeaseAction::Ack);
        assert_eq!(l.until, 1800 + 3600);
        // RENEW с чужим ciaddr -> NAK
        let bogus = client_msg(DHCP_REQUEST, 0, [10, 0, 0, 9], MAC, None, &[]);
        assert_eq!(l.on(&parse(&bogus).unwrap(), 1810, 3600), LeaseAction::Nak);
        assert_eq!(l.state, LeaseState::Idle);
    }

    #[test]
    fn lease_decline_cooldown() {
        let mut l = Lease::idle();
        l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 0, 3600);
        l.on(&req_of(DHCP_REQUEST, MAC, [0; 4], Some(CLIENT_IP)), 2, 3600);
        assert_eq!(l.on(&req_of(DHCP_DECLINE, MAC, [0; 4], None), 10, 3600), LeaseAction::Ignore);
        assert_eq!(l.state, LeaseState::Declined);
        assert_eq!(l.until, 10 + DECLINE_COOLDOWN_SECS); // карантин до t=70
        assert_eq!(l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 20, 3600), LeaseAction::Ignore);
        assert_eq!(l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 69, 3600), LeaseAction::Ignore);
        assert_eq!(l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 71, 3600), LeaseAction::Offer);
        assert_eq!(l.state, LeaseState::Offered);
    }

    #[test]
    fn lease_release_and_discover_restart() {
        let mut l = Lease::idle();
        l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 0, 3600);
        l.on(&req_of(DHCP_REQUEST, MAC, [0; 4], Some(CLIENT_IP)), 2, 3600);
        assert_eq!(l.on(&req_of(DHCP_RELEASE, MAC, CLIENT_IP, None), 100, 3600), LeaseAction::Ignore);
        assert_eq!(l.state, LeaseState::Idle);
        assert_eq!(l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 110, 3600), LeaseAction::Offer);
    }

    #[test]
    fn lease_discover_after_acked_reoffers() {
        let mut l = Lease::idle();
        l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 0, 3600);
        l.on(&req_of(DHCP_REQUEST, MAC, [0; 4], Some(CLIENT_IP)), 2, 3600);
        // ACK продлил аренду до 3602; DISCOVER в момент истечения -> заново OFFER
        assert_eq!(l.on(&req_of(DHCP_DISCOVER, MAC, [0; 4], None), 3602, 3600), LeaseAction::Offer);
        assert_eq!(l.state, LeaseState::Offered);
        assert_eq!(l.until, 3602 + 3600);
    }

    #[test]
    fn lease_idle_informs_and_bare_requests() {
        let mut l = Lease::idle();
        // INFORM от клиента без аренды — игнорируем
        assert_eq!(l.on(&req_of(DHCP_INFORM, MAC, CLIENT_IP, None), 0, 3600), LeaseAction::Ignore);
        // REQUEST без предварительного OFFER — NAK (клиент пойдёт в DISCOVER)
        assert_eq!(l.on(&req_of(DHCP_REQUEST, MAC, [0; 4], Some(CLIENT_IP)), 1, 3600), LeaseAction::Nak);
        assert_eq!(l.state, LeaseState::Idle);
    }
}
