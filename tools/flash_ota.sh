#!/usr/bin/env bash
# Ручная прошивка: положить образ на OTA-сервер и дать плате команду /ota,
# затем дождаться вердикта Valid/Rollback по /api/status.
# Использование: tools/flash_ota.sh [файл]   (по умолчанию dist/app_ota.bin)
#   BOARD=192.168.0.85    — адрес платы (актуальный DHCP-IP видно в коллекторе:
#                           GET http://192.168.0.28/logs -> последние boot-строки)
#   OTA=192.168.0.28:8001 — хост:порт раздачи (VM, каталог /opt/espota)
set -e
BOARD="${BOARD:-192.168.0.85}"
OTA="${OTA:-192.168.0.28:8001}"
FILE="${1:-dist/app_ota.bin}"
# Версия — из прошивки, а не из крейта clock-core: иначе файл на раздаче
# называется app_ota_0.6.10_manual.bin, и URL «не находится» (404).
VER=$(grep -m1 '^version' firmware/Cargo.toml | cut -d'"' -f2)
NAME="app_ota_${VER}_manual.bin"
[ -f "$FILE" ] || { echo "нет файла $FILE" >&2; exit 1; }
echo "локальный sha256:"; sha256sum "$FILE"
echo "загрузка на OTA-хост как $NAME ..."
cat "$FILE" | ssh root@300:31d4:7243:999c::1c "cat > /opt/espota/$NAME && sha256sum /opt/espota/$NAME" 2>/dev/null | grep -v "ioctl\|задания"
echo "команда прошивки платы ..."
curl -s -m 20 "http://$BOARD/ota?url=http://$OTA/$NAME"; echo
echo "ожидаю вердикт (3 мин):"
for _ in 1 2 3 4 5 6 7 8 9 10 11 12; do
    sleep 15
    S=$(curl -s -m 8 "http://$BOARD/api/status" 2>/dev/null)
    V=$(printf '%s' "$S" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('version'), d.get('ota_state'))" 2>/dev/null)
    echo "  ${V:-плата не отвечает}"
    case "$V" in
        *Valid*) echo "ГОТОВО: плата крутит новую прошивку (Valid)"; exit 0 ;;
        *Rollback*) echo "ОТКАТ: новый образ не прошёл — смотри UART/коллектор"; exit 1 ;;
    esac
done
echo "вердикт не дождён; проверь /opt/esplogs/collector.log" >&2
exit 1
