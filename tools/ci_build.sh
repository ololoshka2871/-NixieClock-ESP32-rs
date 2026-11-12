#!/usr/bin/env bash
# Единая сборка прошивки приложения для CI (PR- и release-workflow).
# Ожидает в env: SSID, PASSWORD (секреты репозитория), LOG_URL (переменная).
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

sha256sum dist/app_ota.bin dist/app.elf > dist/sha256.txt
ls -la dist/
