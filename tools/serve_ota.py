#!/usr/bin/env python3
"""Раздача OTA-прошивок для esp32-wifi-bootloader.

Отдаёт файлы из каталога по HTTP с заголовком `X-Crc32`, который проверяет
апдейтер платы (crc32 файла, hex, big-endian — тот же порядок, что читает
updater.rs из заголовка X-Crc32).

Использование:
    python3 tools/serve_ota.py <dir> [--port 8000]
    GET http://<host>:<port>/app_ota.bin
    GET http://<host>:<port>/            — список файлов (проверка, что живой)

Плата: GET http://<board>/ota?url=http://<host>:<port>/app_ota.bin
"""
import argparse
import binascii
import http.server
import io
import os
import sys


def crc32_file(path):
    crc = 0
    with open(path, "rb") as f:
        while chunk := f.read(1 << 16):
            crc = binascii.crc32(chunk, crc)
    return crc & 0xFFFFFFFF


class OtaHandler(http.server.SimpleHTTPRequestHandler):
    def _index(self, path):
        """Список файлов каталога: иначе GET / выглядел как «сервер сломан»."""
        rows = []
        for name in sorted(os.listdir(path)):
            full = os.path.join(path, name)
            if os.path.isfile(full):
                rows.append(f"{os.path.getsize(full):>10}  /{name}")
        text = "OTA-раздача (X-Crc32 включён)\n\n"
        text += "\n".join(rows) if rows else "(каталог пуст)"
        text += "\n"
        body = text.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        return io.BytesIO(body)

    def send_head(self):
        # Полностью свой путь отдачи: заголовки должны уйти ОДНИМ блоком,
        # включая X-Crc32 (у super() end_headers уже вызван).
        if self.command not in ("GET", "HEAD"):
            return super().send_head()
        path = self.translate_path(self.path)
        if os.path.isdir(path):
            return self._index(path)
        if not os.path.isfile(path):
            self.send_error(404, "File not found", f"нет {self.path}")
            return None
        try:
            f = open(path, "rb")
        except OSError as e:
            self.send_error(404, "File not found", str(e))
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
    # ВАЖНО: каталог приводим к абсолютному и НЕ делаем os.chdir.
    # Раньше было `os.chdir(args.dir)` + `directory=args.dir`: при
    # относительном пути (а по умолчанию ota_serve.sh передаёт "dist")
    # SimpleHTTPRequestHandler.translate_path склеивал "dist" + "/app_ota.bin"
    # уже ПОСЛЕ chdir, получалось dist/dist/app_ota.bin — и ЛЮБОЙ файл
    # отдавал 404. С абсолютным путём всё работало, поэтому баг не
    # проявлялся при ручных запусках.
    root = os.path.abspath(args.dir)
    if not os.path.isdir(root):
        sys.exit(f"нет каталога: {root}")
    handler = lambda *a, **kw: OtaHandler(*a, directory=root, **kw)  # noqa: E731
    with http.server.ThreadingHTTPServer(("0.0.0.0", args.port), handler) as httpd:
        print(f"serving {root} on :{args.port} with X-Crc32", flush=True)
        httpd.serve_forever()


if __name__ == "__main__":
    main()
