# NixieClock-ESP32-rs

Часы на Nixie-лампах (ИН-14 ×6, ESP32 classic) — полный порт с C++/ESP-IDF
(Home/NixieClock-ESP32-idf) на Rust: чистые esp-hal / embassy без приложения
ESP-IDF. База OTA/rollback — проверенный шаблон Home/esp32-ota-template
(esp-hal 1.2.2, esp-radio, embassy-net, esp-bootloader-esp-idf с auto-rollback).

**Состояние:** `master` — релизная линия; образ, собранный CI, подтверждён на
железе (тег `v1.1.0`). `legacy-2025` — архивный порт 2025 года (esp-hal
1.0.0-beta.0), не развивается. Мелкие feature/fix-ветки после мержа удаляются.

## Функциональность

### Индикация
- HH:MM:SS на 6 лампах ИН-14: два дешифратора К561ИД1 (аналог CD4028A),
  шина данных A GPIO27/26/25/33 (лампы 1–3) и B GPIO32/12/18/19 (лампы 4–6),
  стробы выбора групп TG0/TG1/TG2 GPIO13/2/14, строб записи GPIO3.
- Все 8 линий данных обновляются одним DMA-переносом I2S1 (LCD-режим,
  16-битные кадры); 3 временных слота на кадр, ~400 Гц на лампу (~832 мкс/слот)
  — плавный кросс-фейд розжига.
- Карта цифра→код построена калибровочным sweep'ом на живом железе; гашение
  «тёмных» слов лампы и blank-пауза между слотами (послесвечение ИН-14).
- Режимы по кнопке BOOT (GPIO0): часы → CO2 → температура → дата; авторвозврат
  к часам через 5 с бездействия.
- Диагностика дисплея из WebUI: тестовые последовательности, тест фейда,
  полное гашение/включение.

### Время
- RTC DS1307 на I2C0 (SDA GPIO21, SCL GPIO22, 400 кГц); часы идут автономно
  без сети.
- Синхронизация по SNTP (раз в 24 ч, при сбое повтор через 60 с; сервер
  настраивается), ручная установка `/api/time`, часовой пояс в минутах
  `/api/tz`.
- `rtcmark` в RTC slow memory: восстановление времени после рестарта, маркеры
  причины запуска и последнего HTTP-запроса.

### Сеть и Wi-Fi
- STA с лестницей переподключений: 10 попыток с быстрым backoff → 20 с медленным
  → 40 с длинным (monotonic backoff, `core/src/diag.rs`).
- После 20 неудач поднимается эвакуационная SoftAP `Nixie-Clock-IN14`
  (192.168.139.1/24, DHCP одному клиенту 192.168.139.100; пароль из
  `SOFTAP_PASS`); та же точка доступа поднимается на старте, если в NVS нет
  валидных кредов. Креды задаются через WebUI (captive-portal POST /wifi) и
  хранятся в NVS.
- Wi-Fi сканер и подбор точки доступа при сбое соединения; ICNP — ответы на
  ping (feature `icnp`).

### WebUI и HTTP API
SPA из коробки прошивки (статика вшита в `core/src/assets`): статус, время/TZ,
NTP, цвет и подсветка, Wi-Fi креды + скан, OTA, логи, диагностика дисплея.

| Метод | Путь | Назначение |
| ----- | ---- | ---------- |
| GET   | `/` | SPA-интерфейс |
| GET   | `/assets/app.js`, `/assets/style.css` | статика SPA |
| GET   | `/api/status` | статус, настройки, датчики, watchdog, версии |
| GET   | `/api/scan` | список Wi-Fi сетей |
| POST  | `/wifi` | сохранение кредов (captive-portal) |
| GET   | `/api/logs?include=all` | кольцо логов платы |
| GET   | `/logs?follow=1` | поток логов |
| POST  | `/api/logs/url` | коллектор логов: `{"url":"http://host:port/logs"}`; `{"url":""}` — выключить |
| GET   | `/api/time` | время платы |
| POST  | `/api/time` | ручная установка времени |
| POST  | `/api/tz` | часовой пояс в минутах |
| POST  | `/ntp/set` | адрес NTP-сервера |
| GET   | `/display/test?mode=…` | тесты индикации |
| POST  | `/display/off`, `/display/on`, `/display/fade` | гашение/включение/фейд-тест |
| GET   | `/ota?url=http(s)://…` | OTA: скачать образ в неактивный слот |
| POST  | `/revert` | manual откат на предыдущий слот |

### OTA и откат
- `GET /ota?url=` (http и https, в т.ч. self-signed) — чанками с проверкой
  erase/program; перед записью гейт: образ должен помещаться в слот
  (≤ 917504 Б, слот 0xE0000).
- Запись в неактивный слот + активация (`otadata`, ESP-IDF bootloader):
  неподтверждённый образ авто-откатывается при падении; ручной откат `/revert`.
- Канонические образы — ассеты релиза: `app_ota.bin` (OTA) и
  `firmware_uart.bin` (полная UART-прошивка с 0x0, таблица 2 MB) + `sha256.txt`.
  Структура образа проверяется `tools/check_ota_image.py`.

### Датчики и питание
- DS18B20 (1-Wire, GPIO4, питание от линии данных через подтяжку 4.7 кОм):
  поиск устройства по ROM, CRC8 скретч-пада, цикл 10 с.
- MH-Z19 (UART2 RX GPIO16 / TX GPIO17, 9600): CO2 в ppm, опрос 10 с, при ошибке
  повтор через 1 с.
- ADC1 (аттенюация 0 dB, калибровка): 3.3 В GPIO39, 5 В GPIO35, 12 В GPIO34;
  опрос 30 с + тест просадки под нагрузкой (1 с полной яркости дисплея).

### Подсветка WS2812
6 светодиодов на GPIO15 (RMT): базовый цвет (месячный паттерн по умолчанию),
эффекты off / breathing / wave / wheel с настраиваемыми частотой и амплитудой
(NVS); 30 мс кадр анимации.

### Устойчивость и диагностика
- hangdetect: ISR TIMG0.timer1 раз в секунду следит за main-циклом; ≥15 с без
  цикла — снимок в RTC slow memory и software_reset раньше 30-с RWDT.
- Panic-hook: panic-инфо в NVS (`settings/last_panic`) + RTC, репорт при
  следующем старте; сигнатуры сброса (RWDT/brownout/panic) в статусе.
- Кольцевой буфер логов + `uarttap`; внешний коллектор POST `…/logs` (url в NVS
  либо `LOG_URL` при сборке).

## Хранилище
- NVS, namespace `settings`: `schema_ver`, `ssid`, `pass`, `ntp_server`,
  `tz_min`, `clock_color`, `led_effect`, `led_freq_pct`, `led_amp_pct`,
  `log_url`, `last_panic`. Данные переживают OTA и рестарты.
- RTC slow memory: `rtcmark` (время, причина запуска, последний HTTP-запрос),
  фрагменты panic.

## Сборка

Тулчейн espup (`cargo +esp`), target `xtensa-esp32-none-elf`
(см. [firmware/.cargo/config.toml](firmware/.cargo/config.toml)).

```bash
bash tools/ci_build.sh
# dist/app.elf  dist/app_ota.bin  dist/firmware_uart.bin  dist/sha256.txt
```

- `SSID`/`PASSWORD`/`LOG_URL` — env при сборке (необязательны). Без кредов
  плата поднимает setup-SoftAP — Wi-Fi задаётся через WebUI. Пароль AP —
  `SOFTAP_PASS` (CI-секрет либо `[env]` в `firmware/.cargo/config.toml`);
  пустой экспортированный секрет глушит SoftAP, поэтому в CI он либо задан,
  либо unset.
- В линковку включён `-Wl,--no-eh-frame-hdr`: unwind-заголовок (~12.8 КБ) не
  нужен собственному `#[panic_handler]`, без него образ не помещается в слот.
- Хост-тесты ядра: `cargo test -p clock-core` (x86, в CI на каждый PR).

### CI
- PR → `.gitea/workflows/esp32-build.yml`: хост-тесты + сборка + гейт образа.
- Тег `v*` → `.gitea/workflows/esp32-release.yml`: релиз Gitea с ассетами
  `app_ota.bin`, `firmware_uart.bin`, `app.elf`, `sha256.txt`.

## Прошивка платы

- **UART (первичная/аварийная):** `espflash write-bin 0x0 dist/firmware_uart.bin`
  (полный образ: bootloader + таблица 2 MB + приложение в ota_0).
- **OTA:** образ берётся из ассета релиза (`…/releases/download/v…/app_ota.bin`),
  кладётся на OTA-сервер (`AAA.BBB.CCC.DDD:8000`, `tools/ota_serve.sh` /
  `tools/serve_ota.py`) и тянется платой: `GET /ota?url=…` или кнопкой WebUI.
  Падение неподтверждённого образа — авто-откат bootloader'а; ручной вариант —
  `POST /revert`.
- Креды в NVS без сети: `python3 tools/inject_creds.py <порт>` (SOFTAP_PASS).

## Структура (cargo workspace)

- `core/` (`clock-core`) — `no_std` логика без железа: индикация (карты, слоты,
  гашение), время (DS1307, SNTP, TZ), настройки (NVS-схема), эффекты LED,
  SoftAP/DHCP, диагностика/backoff, сборщики HTML, assets WebUI. Хост-тесты.
- `firmware/` (`nixieclock-firmware`) — esp-бинарь: tasks (main, app, clock,
  display, sensors, softap, server, updater, hangdetect, watchdog, crashlog,
  uarttap, tz, rtcmark, flashcell, power, led, ws2812, ui, netdev, dhcp, icmp).
- `tools/` — `ci_build.sh`, `check_ota_image.py`, `inject_creds.py`,
  `flash_ota.sh`, `ota_serve.sh`, `serve_ota.py`, `collector.py` (сборщик логов),
  `lolkntp.py` (NTP-сервер).
- `partitions_2mb.csv` — фактическая разметка платы (nvs/otadata/
  ota_0@0x20000/ota_1@0x100000, слоты 0xE0000); `partitions_4mb.csv` — эталон
  для 4 MB.
- `HARDWARE.md` — паспорт железа: пиныport-версии, электрика, разметка flash;
  `Schematic-lv.png` — оригинальная схема.

## Железо

ESP32 classic v1.0 (min revision 1.0.0), 2 MB flash, без PSRAM, сдвиговых
регистров нет. Полные пины этой версии, электрика дешифраторов (К561ИД1,
+4.6 В), стробы катодов TG, перемычка D2B→GPIO12, датчики и ADC —
в [HARDWARE.md](HARDWARE.md).
