#!/usr/bin/env python3
"""ESP32 WiFi log collector (hardened).

POST /logs            -> append (prefixes source IP)
GET  /logs            -> tail of last 64 KB
GET  /logs?follow=1   -> live stream, 2 s heartbeat ('# tick'), max 1 h, max 2
                         concurrent streams per IP — dead clients are killed by
                         heartbeat write (EPIPE), threads never leak.
Every connection has a 15 s socket timeout: a POST aborted mid-body (board
crashed while sending) no longer wedges a handler thread forever.
Log rotation: collector.log > 20 MB -> collector.log.1
"""
import http.server
import os
import socket
import threading
import time

LOGF = "/opt/esplogs/collector.log"
MAXLOG = 20 * 1024 * 1024
MAX_POST = 1024 * 1024
MAXFOLLOW_IP = 2
MAXFOLLOW_LIFE = 3600
IO_TIMEOUT = 15

_follows = {}
_lock = threading.Lock()
_post_cnt = 0


def _rotate_if_needed():
    global _post_cnt
    _post_cnt += 1
    if _post_cnt % 256 == 0:
        try:
            if os.path.getsize(LOGF) > MAXLOG:
                os.replace(LOGF, LOGF + ".1")
        except OSError:
            pass


class H(http.server.BaseHTTPRequestHandler):
    timeout = IO_TIMEOUT

    def handle(self):
        try:
            super().handle()
        except (socket.timeout, TimeoutError, ConnectionError, BrokenPipeError, OSError):
            pass

    def do_POST(self):
        try:
            n = int(self.headers.get("content-length") or 0)
        except ValueError:
            n = -1
        if n <= 0 or n > MAX_POST:
            self.send_error(400)
            return
        data = self.rfile.read(n)
        src = self.client_address[0].encode()
        with open(LOGF, "ab") as f:
            for line in data.splitlines():
                f.write(src + b" -> " + line + b"\n")
        _rotate_if_needed()
        self.send_response(200)
        self.send_header("Content-Length", "2")
        self.end_headers()
        self.wfile.write(b"ok")

    def do_GET(self):
        ip = self.client_address[0]
        if "follow" in self.path:
            with _lock:
                c = _follows.get(ip, 0)
                if c >= MAXFOLLOW_IP:
                    self.send_error(503)
                    return
                _follows[ip] = c + 1
            try:
                self._follow()
            finally:
                with _lock:
                    _follows[ip] = max(0, _follows.get(ip, 1) - 1)
            return
        body = b""
        if os.path.exists(LOGF):
            with open(LOGF, "rb") as f:
                f.seek(0, 2)
                sz = f.tell()
                f.seek(max(0, sz - 65536))
                body = f.read()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _follow(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        deadline = time.monotonic() + MAXFOLLOW_LIFE
        with open(LOGF, "rb") as f:
            f.seek(0, 2)
            while time.monotonic() < deadline:
                data = f.read()
                if not data:
                    data = b"# tick\n"
                self.wfile.write(data)
                self.wfile.flush()
                time.sleep(2)

    def log_message(self, *a):
        pass


class S(http.server.ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


if __name__ == "__main__":
    S(("0.0.0.0", 80), H).serve_forever()
