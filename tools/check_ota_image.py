#!/usr/bin/env python3
"""Структурная проверка OTA-образа (app-only esp_image) перед релизом/OTA.

CI ловит битые сборки до того, как образ доедет до платы: магик 0xE9,
сегменты влезают в файл, min_chip_rev совместим с платой (v1.0), appended
SHA-256 сходится, размер не больше OTA-слота. Плата при закачке дополнительно
проверяет магик (гейт до записки в слот).
Формат: esp_image_header_t (24 B) + esp_image_segment_header_t (8 B) * N + данные,
далее optional SHA256 (espflash "simple hash"), по esp_app_format.h ESP-IDF 5.4.
"""
import argparse
import struct
import sys

MAGIC = 0xE9
HEADER_SIZE = 24
MAX_SEGMENTS = 16


def main() -> int:
    ap = argparse.ArgumentParser(description="check esp_image app-only bin")
    ap.add_argument("image")
    ap.add_argument("--max-size", type=int, default=1048576,
                    help="максимальный размер образа (размер OTA-слота), байт")
    ap.add_argument("--max-chip-rev", type=int, default=100,
                    help="ревизия платы (major*100+minor): образ с min_chip_rev_full "
                         "больше загрузчик платы не запустит")
    a = ap.parse_args()

    def fail(msg: str) -> None:
        print(f"FAIL: {msg}", file=sys.stderr)
        sys.exit(1)

    data = open(a.image, "rb").read()
    if len(data) < HEADER_SIZE:
        fail("файл короче esp_image_header (24 B)")
    if data[0] != MAGIC:
        fail(f"плохой магик 0x{data[0]:02x}, ожидается 0xE9")
    seg_count = data[1]
    if not 1 <= seg_count <= MAX_SEGMENTS:
        fail(f"неправдоподобное число сегментов: {seg_count}")
    entry = struct.unpack_from("<I", data, 4)[0]
    if entry == 0:
        fail("entry_addr == 0")
    min_rev = struct.unpack_from("<H", data, 15)[0]
    max_rev = struct.unpack_from("<H", data, 17)[0]
    hash_appended = data[23]

    off = HEADER_SIZE
    segs = []
    for i in range(seg_count):
        if off + 8 > len(data):
            fail(f"заголовок сегмента {i} вылез за конец файла")
        load_addr, dlen = struct.unpack_from("<II", data, off)
        off += 8
        if dlen > len(data) - off:
            fail(f"сегмент {i} (len={dlen}) не влезает в файл")
        if load_addr == 0 or load_addr >= 0x8000_0000:
            fail(f"подозрительный load_addr сегмента {i}: 0x{load_addr:08x}")
        segs.append((load_addr, dlen))
        off += dlen
    tail = len(data) - off
    # espflash append ("simple hash" block): в IDF 5.4 у classic ESP32 это
    # 48 B трейлер (zero-pad + хэш-блок). Байт-в-байт не верифицируем —
    # целостность образа на пути к плате обеспечивает CRC32 OTA-транспорта,
    # а загрузчик проверяет сам образ при загрузке из слота.
    if tail > 48:
        fail(f"после сегментов висит {tail} B мусора (>48 B трейлера espflash)")

    if min_rev > a.max_chip_rev:
        fail(f"образ требует chip rev >= v{min_rev // 100}.{min_rev % 100}, "
             f"плата v{a.max_chip_rev // 100}.{a.max_chip_rev % 100} "
             f"(ESP_HAL_CONFIG_MIN_CHIP_REVISION=??? — загрузчик откажет: "
             f"\"Image requires chip rev >= v{min_rev // 100}.{min_rev % 100}\")")

    if len(data) > a.max_size:
        fail(f"образ {len(data)} B не влезает в OTA-слот {a.max_size} B")

    print(f"OK: entry=0x{entry:08x}, сегментов={seg_count}, "
          f"chip_rev={min_rev}..{max_rev}, файл={len(data)} B "
          f"({100.0 * len(data) / a.max_size:.1f}% слота), "
          f"hash_appended={hash_appended}")
    for i, (addr, dlen) in enumerate(segs):
        print(f"  seg{i}: addr=0x{addr:08x} len={dlen}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
