#!/usr/bin/env bash
# Поднять HTTP-сервер раздачи образов прошивки (для ручного цикла «собрал -> зашил»).
# Использование: tools/ota_serve.sh [dir] [port]   (по умолчанию dist/ на :8001)
# Плата должна иметь доступ к этому хосту по LAN.
set -e
DIR="${1:-dist}"
PORT="${2:-8001}"
if [ ! -f "$DIR/app_ota.bin" ]; then
    echo "warning: $DIR/app_ota.bin не найден — собери сначала (tools/ci_build.sh)" >&2
fi
sha256sum "$DIR"/*.bin 2>/dev/null || true
echo "раздача $DIR на 0.0.0.0:$PORT; адрес(а) для платы:"
for ip in $(hostname -I 2>/dev/null); do
    echo "  http://$ip:$PORT/app_ota.bin"
done
echo "прошивка платы (пример):"
echo "  curl \"http://192.168.0.85/api/ota?url=http://<IP>:$PORT/app_ota.bin\""
exec python3 -m http.server "$PORT" --directory "$DIR" --bind 0.0.0.0
