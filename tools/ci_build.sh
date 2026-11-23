#!/usr/bin/env bash
# Единая сборка прошивки приложения для CI (PR- и release-workflow).
# Ожидает в env: SSID, PASSWORD (секреты репозитория), LOG_URL (переменная),
# SOFTAP_PASS (секрет; без него main.rs оставляет softap_creds=None и
# SoftAP-эвакуация выключена — в логе сборки это видно по NOTE ниже).
# Пустые SSID/PASSWORD снимаются -> в дело идут плейсхолдеры из
# firmware/.cargo/config.toml (CI гарантированно собираем и без секретов).
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

# Гат кредал: env!("SSID") и env!("PASSWORD") склеены в rodata в один
# printable-забег. Если CI потерял секреты, вместо них плейсхолдеры из
# config.toml -> плата уходит offline после бутa (печатаем только длины).
if [ -n "${SSID:-}" ]; then
  cred_tail=$(tr -c '[:print:]' '\n' < dist/app_ota.bin | sed -n "s|^${SSID}||p" | head -1 | tr -d '\r\n')
  if [ "${#cred_tail}" != "${#PASSWORD}" ]; then
    echo "FATAL: baked-in station password len=${#cred_tail}, expected ${#PASSWORD}"
    exit 1
  fi
  echo "gate: station credential baked in (len=${#cred_tail})"
else
  echo "NOTE: SSID secret empty -> build carries config.toml placeholders"
fi
if [ -n "${SOFTAP_PASS:-}" ]; then
  echo "gate: softap evacuation ENABLED (len=${#SOFTAP_PASS})"
else
  echo "NOTE: SOFTAP_PASS secret not set -> SoftAP evacuation compiled out"
fi

sha256sum dist/app_ota.bin dist/app.elf dist/firmware_uart.bin > dist/sha256.txt
ls -la dist/
