# NixieClock-ESP32-rs

Порт часов на Nixie-лампах (ESP32) с C++/ESP-IDF (Home/NixieClock-ESP32-idf) на Rust.
OTA/rollback-база — проверенный шаблон Home/esp32-ota-template (esp-hal 1.2.2, esp-radio, embassy-net, esp-bootloader-esp-idf с auto-rollback).

Ветка `legacy-2025` — прежний незавершённый порт (esp-hal 1.0.0-beta.0). Ветка `rewrite` — текущая работа.

## Структура (cargo workspace)
- `core/` (`clock-core`) — no_std бизнес-логика без железа: модель времени, настройки, framebuffer дисплея, кодирование ламп. Хост-тесты: `cargo test -p clock-core` (x86, в CI).
- `firmware/` (`nixieclock-firmware`) — esp-бинарь: WiFi STA+fallback AP, HTTP-страница настроек, OTA с rollback-guard, логи по WiFi, RTC DS1307, датчики, драйвер индикации.
- `tools/ci_build.sh` — сборка + `espflash save-image` (app-only) + проверка OTA-образа (гейт размера слота 0xE0000 — флеш платы 2MB).
- `partitions_2mb.csv` — фактическая разметка платы (nvs/otadata/ota_0@0x20000/ota_1@0x100000, слоты 0xE0000); `partitions_4mb.csv` — эталонная 4MB.
- `HARDWARE.md`, `Schematic-lv.png` — паспорт железа оригинальных часов (пины, схема).

## Железо (ESP32 classic v1.0, 2MB flash, без PSRAM, без сдвиговых регистров)
- Индикация: прямые GPIO — шина A GPIO27/26/25/33 (лампы 1–3), шина B GPIO32/12/18/19 (лампы 4–6), стробы анодов TG GPIO13/2/14 (one-hot, 3 группы). Дешифраторы 74141/4-в-16, карта кодов: 0→3,1→2,2→4,3→1,4→8,5→13,6→5,7→12,8→9,9→0, гашение→10. Стратегия порта: DMA-обновление 8 линий данных (LCD-mode 8 lanes + WS), стробы — по результату probe.
- RTC DS1307: I2C SDA=21 SCL=22, SQW 1Гц → GPIO23. DS18x20: GPIO4. MH-Z19 CO2: UART2 TX=17 RX=16 @9600. NeoPixel 6px: GPIO15. Кнопка: GPIO0. Напряжения 3.3/5/12V: ADC GPIO39/35/34.
- Консоль/логи: UART0 (TX1/RX3) + POST на коллектор http://192.168.0.28/logs.

## Сборка
Тулчейн espup (`cargo +esp`), target xtensa-esp32-none-elf (см. firmware/.cargo/config.toml).
SSID/PASSWORD/LOG_URL — через env ([env] в config.toml — плейсхолдеры для локальных сборок; реальные креды инжектит CI).
`bash tools/ci_build.sh` → dist/app.elf + dist/app_ota.bin + dist/sha256.txt.

## Выкатка на плату (2MB-разметка уже в flash, загрузчик с rollback прошит)
1. CI/локально собирается app-only образ (magic E9, ≤ 917504 B).
2. Кладётся на OTA-сервер (VM 192.168.0.28:8000).
3. Плата (192.168.0.123): `GET /ota?url=...` (текущее демо) или `POST /api/ota {"url":...}` (новая прошивка) → слот → PendingVerify → confirm после диагностики → Valid; падение — авто-откат.

## roadmap
M0 каркас/CI → M1 сеть+HTTP+HTML-страница → M2 OTA нового приложения + rollback → M3 NVS+кнопка → M4 DS1307+NTP → M5 индикация (DMA/probe-результат) → M6 датчики.
