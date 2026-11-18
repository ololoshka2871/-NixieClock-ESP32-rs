#!/usr/bin/env python3
"""Раздача OTA-прошивок для esp32-wifi-bootloader.

Отдаёт файлы из каталога по HTTP с заголовком `X-Crc32`, который проверяет
апдейтер платы (crc32 файла, hex, big-endian — тот же порядок, что читает
updater.rs из заголовка X-Crc32).

Использование:
    python3 tools/serve_ota.py <dir> [--port 8000]
    GET http://<host>:<port>/firmware_rollback_2mb.bin

Плата: GET http://<board>/ota?url=http://<host>:<port>/firmware_rollback_2mb.bin
"""
import argparse
import binascii
import http.server
import os
import sys


def crc32_file(path):
    crc = 0
    with open(path, "rb") as f:
        while chunk := f.read(1 << 16):
            crc = binascii.crc32(chunk, crc)
    return crc & 0xFFFFFFFF


class OtaHandler(http.server.SimpleHTTPRequestHandler):
    def send_head(self):
        # Полностью свой путь отдачи: заголовки должны уйти ОДНИМ блоком,
        # включая X-Crc32 (у super() end_headers уже вызван).
        if self.command not in ("GET", "HEAD"):
            return super().send_head()
        path = self.translate_path(self.path)
        if not os.path.isfile(path):
            self.send_error(404, "File not found")
            return None
        try:
            f = open(path, "rb")
        except OSError:
            self.send_error(404, "File not found")
            return None
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(os.fstat(f.fileno()).st_size))
        self.send_header("X-Crc32", format(crc32_file(path), "08x"))
        self.end_headers()
        return f

    def log_message(self, format, *args):
        sys.stderr.write("[ota-srv] %s - %s\n" % (self.address_string(), format % args))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dir")
    ap.add_argument("--port", type=int, default=8000)
    args = ap.parse_args()
    os.chdir(args.dir)
    handler = lambda *a, **kw: OtaHandler(*a, directory=args.dir, **kw)  # noqa: E731
    with http.server.ThreadingHTTPServer(("0.0.0.0", args.port), handler) as httpd:
        print(f"serving {args.dir} on :{args.port} with X-Crc32", flush=True)
        httpd.serve_forever()


if __name__ == "__main__":
    main()
