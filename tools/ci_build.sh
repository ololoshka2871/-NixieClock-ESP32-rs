#!/usr/bin/env bash
# Единая сборка прошивки приложения для CI (PR- и release-workflow).
# Ожидает в env: SSID, PASSWORD (секреты репозитория, оба необязательны),
# LOG_URL (переменная), SOFTAP_PASS (секрет либо fallback из [env] config.toml).
# Пустой SSID — нормальный сценарий: образ стартует в setup-SoftAP, чтобы
# настроить Wi-Fi через WebGUI. Без секретов сборка тоже проходит.
set -euo pipefail
export PATH="/root/.cargo/bin:$HOME/.espflash/bin:$PATH"
# xtensa gcc из espup-тулчейна (CI-образ задаёт PATH через ENV, dev-контейнер — нет)
[ -f /root/export-esp.sh ] && . /root/export-esp.sh
if [ -z "${SSID:-}" ]; then unset SSID PASSWORD 2>/dev/null || true; fi

cd "$(dirname "$0")/.."
PKG=$(cargo metadata --no-deps --format-version 1 | python3 -c \
  'import json,sys; print(next(p["name"] for p in json.load(sys.stdin)["packages"] if p["manifest_path"].endswith("/firmware/Cargo.toml")))')

( cd firmware && cargo +esp build --release )
# workspace: артефакты в target/ корня workspace, не firmware/target
TARGET=$(cargo metadata --no-deps --format-version 1 | python3 -c \
  'import json,sys; print(json.load(sys.stdin)["target_directory"])')
ELF="$TARGET/xtensa-esp32-none-elf/release/$PKG"
[ -f "$ELF" ] || { echo "no ELF: $ELF"; exit 1; }

mkdir -p dist
cp "$ELF" dist/app.elf

# OTA-образ: app-only (кладётся в OTA-слот через /ota на плате; merged не нужен —
# загрузчик с rollback уже прошит в flash один раз, см. README).
# Размер держим за счёт -Wl,--no-eh-frame-hdr из firmware/.cargo/config.toml:
# без него .eh_frame_hdr добавляет ~12.8 КБ и образ не лезет в 917504 B.
espflash save-image --chip esp32 "$ELF" dist/app_ota.bin
od -A x -N 4 dist/app_ota.bin
python3 tools/check_ota_image.py dist/app_ota.bin --max-size 917504

# Merged UART-образ: bootloader + 2MB-таблица + app в слоте ota_0;
# пишется целиком: espflash write-bin 0x0 firmware_uart.bin
# (otadata после такой прошивки стёрта — приложение само выставляет
# selected по реально загруженному слоту на первом буте).
espflash save-image --chip esp32 --merge --skip-padding \
  --partition-table partitions_2mb.csv "$ELF" dist/firmware_uart.bin
od -A x -j 0x8000 -N 2 dist/firmware_uart.bin

# Гат кредал: при наличии секрета SSID/PASSWORD они склеены в rodata в один
# printable-забег (печатаем только длины). Без секрета SSID пустой — это не
# ошибка: плата поднимет setup-SoftAP и примет креды через WebGUI.
if [ -n "${SSID:-}" ]; then
  cred_tail=$(tr -c '[:print:]' '\n' < dist/app_ota.bin | sed -n "s|^${SSID}||p" | head -1 | tr -d '\r\n')
  if [ "${#cred_tail}" != "${#PASSWORD}" ]; then
    echo "FATAL: baked-in station password len=${#cred_tail}, expected ${#PASSWORD}"
    exit 1
  fi
  echo "gate: station credential baked in (len=${#cred_tail})"
else
  echo "NOTE: no SSID secret -> device will boot into setup SoftAP"
fi
if [ -n "${SOFTAP_PASS:-}" ]; then
  echo "gate: setup/evacuation AP password from CI secret (len=${#SOFTAP_PASS})"
else
  echo "NOTE: SOFTAP_PASS secret empty -> using [env] fallback from config.toml"
fi

sha256sum dist/app_ota.bin dist/app.elf dist/firmware_uart.bin > dist/sha256.txt
ls -la dist/
