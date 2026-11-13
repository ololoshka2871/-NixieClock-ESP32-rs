#!/usr/bin/env python3
"""Minimal SNTP server (mode 4) for LAN clients. Unprivileged LXC has no
adjtimex, so system-time daemons can not run here; the container clock is
host-synchronized and accurate, we just answer queries with it.

Wire format matches core/src/time.rs::sntp_parse: 48-byte packet, seconds
of the NTP era as u32 at the high half of each 8-byte timestamp field
(Reference 16, Originate 24, Receive 32, Transmit 40)."""
import socket, struct, time

NTP_EPOCH = 2208988800  # seconds between 1900 and 1970

def ts4():
    # seconds of current NTP era (u32), era bit 0 — fine for decades
    return (int(time.time()) + NTP_EPOCH) & 0xFFFFFFFF

s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("0.0.0.0", 123))
while True:
    data, peer = s.recvfrom(1024)
    if len(data) < 48 or (data[0] & 0x07) != 3:
        continue  # only NTP client-mode requests
    out = bytearray(48)
    out[0] = 0x24                     # LI=0 VN=4 mode=4
    out[1] = 2                        # stratum 2 (reference = container clock)
    out[2] = 6                        # poll 64 s
    out[3] = 0xEC                     # precision -20 (~1 us)
    out[12:16] = b"LOLK"              # reference id
    sec = ts4()
    struct.pack_into(">I", out, 16, sec)   # reference timestamp
    out[24:32] = data[40:48]               # originate = client's transmit (echo)
    struct.pack_into(">I", out, 32, sec)   # receive timestamp
    struct.pack_into(">I", out, 40, sec)   # transmit timestamp
    s.sendto(bytes(out), peer)
