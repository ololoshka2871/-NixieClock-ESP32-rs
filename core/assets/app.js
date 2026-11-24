"use strict";
const $ = id => document.getElementById(id);
const fmtU = s => { const d = Math.floor(s / 86400), h = Math.floor(s % 86400 / 3600), m = Math.floor(s % 3600 / 60); return (d ? d + "д " : "") + h + "ч " + m + "м " + (s % 60) + "с" };
async function j(u) { const r = await fetch(u, { cache: "no-store" }); return r.json() }
async function poll() {
    try {
        const s = await j("/api/status");
        $("uptime").textContent = fmtU(s.uptime);
        $("fw").textContent = s.fw + " v" + s.version;
        $("ip").textContent = s.ip || (s.ap_active ? "SoftAP поднимается…" : "нет IP (wifi не подключён)");
        $("gw").textContent = s.ap_active
            ? "нет (режим SoftAP)"
            : (s.gateway || (s.dhcp_done ? "нет (роутер не передаёт шлюз)" : "нет (DHCP не завершён)"));
        $("dns").textContent = s.ap_active
            ? "нет (режим SoftAP)"
            : (s.dns || (s.dhcp_done ? "нет (роутер не передаёт DNS)" : "нет (DHCP не завершён)"));
        $("sensor_temp").textContent = s.temp_mc == null ? "нет данных" : (s.temp_mc / 1000).toFixed(1) + " °C";
        $("sensor_co2").textContent = s.co2_ppm == null ? "нет данных" : s.co2_ppm + " ppm";
        $("sensor_v33").textContent = s.v33_mv == null ? "нет данных" : (s.v33_mv / 1000).toFixed(2) + " В";
        $("sensor_v5").textContent = s.v5_mv == null ? "нет данных" : (s.v5_mv / 1000).toFixed(2) + " В";
        $("sensor_v12").textContent = s.v12_mv == null ? "нет данных" : (s.v12_mv / 1000).toFixed(2) + " В";
        $("wifi_cur").textContent = s.ap_active ? "точка доступа (setup)" : (s.wifi_ssid || "—");
        $("wifi_src").textContent = s.ap_active
            ? "SoftAP: откройте http://192.168.139.1 и сохраните Wi-Fi"
            : (s.wifi_ready
                ? (s.wifi_saved ? "из настроек" : "из прошивки")
                : "нет валидных кредов — часы работают без сети");
        let atxt = String(s.wifi_attempts) + " попыток";
        if (s.wifi_fails > 0) {
            atxt += ", неудач подряд " + s.wifi_fails + ", пауза " + s.wifi_backoff_secs + " с";
        }
        $("wifi_conn_att").textContent = atxt;
        $("wifi_scan_seen").textContent = s.wifi_target_seen
            ? "мишень видна в скане"
            : (s.wifi_scan_seen_any ? "цель не найдена в скане" : "скан не дал ни одной AP");
        const el = $("ota_state"); el.textContent = s.ota_state + (s.pending ? " (ждет подтверждения)" : "");
        el.className = s.ota_state === "Valid" ? "ok" : (s.pending || s.ota_state === "New" ? "warn" : "err");
        $("time_now").textContent = s.time || "—";
        $("tz_cur").textContent = tzLabel(s.tz_min);
        if (!tzTouched && document.activeElement !== $("tz_in")) $("tz_in").value = String(s.tz_min);
        $("time_src").textContent = (s.time_source || "?") + (s.time_synced ? "" : " (не подтверждено)");
        $("ntp_cur").textContent = s.ntp_server || "пулы по умолчанию";
        $("logurl_cur").textContent = s.log_url || "не настроен — логи только по UART/кольцу";
        $("led_color").textContent = s.led_color || "—";
        $("led_month").textContent = s.led_color_month || "—";
        $("led_effect").textContent = s.led_effect || "—";
        $("led_freq").textContent = (s.led_freq_pct == null ? "—" : s.led_freq_pct + " %");
        $("led_amp").textContent = (s.led_amp_pct == null ? "—" : s.led_amp_pct + " %");
        $("led_mode").textContent = s.mode || "—";
        syncEffectTuning(s);
        if (!ledTouched && document.activeElement !== $("led_pick")) {
            $("led_pick").value = (s.led_color_set && s.led_color) ? s.led_color : (s.led_color_month || "#000000");
        }
    } catch (e) { $("uptime").textContent = "сервер недоступен" }
}
poll(); setInterval(poll, 3000);
$("ota_btn").onclick = async () => {
    const u = $("ota_url").value.trim(); if (!u) return;
    $("ota_msg").textContent = "запрошено…";
    try { const r = await fetch("/ota?url=" + encodeURIComponent(u)); $("ota_msg").textContent = "Выполняется" } catch (e) { $("ota_msg").textContent = "ошибка: " + e }
};
$("revert_btn").onclick = async () => {
    if (!confirm("Пометить текущий образ INVALID и перезагрузиться (откат на старый слот)?")) return;
    try { const r = await fetch("/revert"); $("ota_msg").textContent = await r.text() } catch (e) { $("ota_msg").textContent = "сброс выполнен, ждите перезагрузки" }
};
$("log_btn").onclick = async () => {
    try { const r = await fetch("/logs", { cache: "no-store" }); $("logs").textContent = await r.text() } catch (e) { $("logs").textContent = "ошибка: " + e }
};
// Коллектор логов — необязательный: пустое значение отключает отправку.
async function setLogUrl(url) {
    $("logurl_msg").textContent = "сохраняю…";
    try {
        const r = await fetch("/api/logs/url", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ url }) });
        $("logurl_msg").textContent = await r.text();
    } catch (e) { $("logurl_msg").textContent = "ошибка: " + e }
}
$("logurl_btn").onclick = () => setLogUrl($("logurl_in").value.trim());
$("logurl_off").onclick = () => setLogUrl("");
$("wifi_btn").onclick = async () => {
    const ssid = $("wifi_ssid_in").value.trim(), pass = $("wifi_pass_in").value;
    $("wifi_msg").textContent = "сохраняю…";
    try {
        const r = await fetch("/api/wifi/connect", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ssid, password: pass }) });
        const j = await r.json();
        $("wifi_msg").textContent = j.status === "saved" ? "сохранено в NVS, перезагрузка…" : "ошибка: " + (j.error || r.status);
    } catch (e) { $("wifi_msg").textContent = "ошибка: " + e }
};

// ---- часовой пояс: смещение от UTC в минутах ----
const MOSCOW = 180;
function tzLabel(min) {
    if (min == null) return "—";
    const a = Math.abs(min), h = Math.floor(a / 60), m = a % 60;
    const s = "UTC" + (min < 0 ? "-" : "+") + String(h).padStart(2, "0") + ":" + String(m).padStart(2, "0");
    return min === MOSCOW ? s + " (Москва)" : s;
}
// Шаг 30 мин по всему диапазону UTC-12:00…UTC+14:00 плюс получасовые/45-минутные пояса.
function tzOffsets() {
    const out = [];
    for (let m = -720; m <= 840; m += 30) out.push(m);
    for (const m of [345, 570, 765]) if (!out.includes(m)) out.push(m);
    return out.sort((a, b) => a - b);
}
let tzTouched = false;
(function fillTz() {
    const sel = $("tz_in");
    for (const m of tzOffsets()) {
        const o = document.createElement("option");
        o.value = String(m); o.textContent = tzLabel(m);
        sel.appendChild(o);
    }
    sel.value = String(MOSCOW);
    sel.addEventListener("change", () => { tzTouched = true; $("tz_msg").textContent = "" });
})();
$("tz_btn").onclick = async () => {
    const tz_min = parseInt($("tz_in").value, 10);
    $("tz_msg").textContent = "сохраняю…";
    try {
        const r = await fetch("/api/tz", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ tz_min }) });
        const k = await r.json();
        if (k.status === "saved") {
            tzTouched = false;
            $("tz_msg").textContent = "пояс " + tzLabel(k.tz_min) + " сохранён, часы показывают " + k.time;
        } else {
            $("tz_msg").textContent = "ошибка: " + (k.error || r.status);
        }
        poll();
    } catch (e) { $("tz_msg").textContent = "ошибка: " + e }
};

$("time_btn").onclick = async () => {
    const v = $("time_set_in").value;
    if (!v) { $("time_msg").textContent = "выберите дату и время (UTC, пояс применяется только к показу)"; return; }
    const ep = Math.floor(Date.parse(v + "Z") / 1000); // datetime-local трактуем как UTC
    if (isNaN(ep)) { $("time_msg").textContent = "не читается дата"; return; }
    $("time_msg").textContent = "устанавливаю…";
    try {
        const r = await fetch("/api/time", { method: "POST", body: "epoch=" + ep });
        const j = await r.json();
        $("time_msg").textContent = r.ok ? ("установлено: " + j.time + " (локальное)") : ("ошибка: " + (r.status));
        poll();
    } catch (e) { $("time_msg").textContent = "ошибка: " + e }
};

$("ntp_btn").onclick = async () => {
    const server = $("ntp_in").value.trim(); // пусто = сброс на пулы по умолчанию
    $("ntp_msg").textContent = "сохраняю…";
    try {
        const r = await fetch("/api/ntp", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ server }) });
        const k = await r.json();
        $("ntp_msg").textContent = k.status === "saved"
            ? ("сохранено: " + k.server + " — применится в следующем цикле SNTP (≤60 с)")
            : ("ошибка: " + (k.error || r.status));
        poll();
    } catch (e) { $("ntp_msg").textContent = "ошибка: " + e }
};

// ---- диагностика индикации ----
async function getText(u) { const r = await fetch(u, { cache: "no-store" }); return await r.text() }
async function kv(u) {
    const t = await getText(u), o = {};
    t.split("\n").forEach(l => { const i = l.indexOf("="); if (i > 0) o[l.slice(0, i)] = l.slice(i + 1) });
    return o;
}
const digits = $("digits");
for (let d = 0; d <= 9; d++) {
    const b = document.createElement("button");
    b.textContent = d;
    b.className = "sec";
    b.title = "цифра " + d + " на выбранной лампе";
    b.onclick = () => testDigit(d);
    digits.appendChild(b);
}
async function testDigit(d) {
    const u = "/display/test?digit=" + d + "&lamp=" + $("lamp_sel").value + "&nostrobe=" + ($("nostrobe").checked ? 1 : 0);
    await getText(u);
    pollDiag();
}
$("d_clock").onclick = async () => {
    await getText("/display/test?off=1&nostrobe=0");
    $("nostrobe").checked = false;
    await getText("/display/on");
    pollDiag();
};
$("d_off").onclick = async () => { await getText("/display/off"); pollDiag() };
$("d_cycle").onclick = async () => { await getText("/display/test?cycle=1&lamp=6"); pollDiag() };
$("d_raw").onclick = async () => { await getText("/display/test?rawcycle=1&lamp=6"); pollDiag() };
$("fade").onchange = async e => {
    await getText("/display/fade?on=" + (e.target.checked ? 1 : 0));
    pollDiag();
};

// Индикация: держим в синхроне только переключатель fade — остальное
// (питание, самопроверка, детальные счётчики) в WebUI не нужно.
async function pollDiag() {
    let s;
    try { s = await kv("/status") } catch (e) { return }
    if (document.activeElement !== $("fade")) $("fade").checked = s.display_fade === "true";
}
pollDiag();
setInterval(pollDiag, 3000);

// ---- подсветка часов ----
let ledTouched = false;
$("led_pick").addEventListener("change", () => { ledTouched = true });
$("led_apply").onclick = async () => {
    const hex = $("led_pick").value;
    try {
        const r = await fetch("/api/color", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ hex }) });
        const k = await r.json();
        ledTouched = false;
        poll();
    } catch (e) { alert("ошибка: " + e) }
};
$("led_reset").onclick = async () => {
    try {
        const r = await fetch("/api/color", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ reset: true }) });
        const k = await r.json();
        ledTouched = false;
        poll();
    } catch (e) { alert("ошибка: " + e) }
};
// ---- частота и амплитуда подсветки ----
let ledFxTouched = false;
let ledTuningTouched = false;
const ledFx = $("led_fx");
const ledFreq = $("led_freq_in");
const ledAmp = $("led_amp_in");

function syncLocalTuning() {
    $("led_freq_out").textContent = ledFreq.value + "%";
    $("led_amp_out").textContent = ledAmp.value + "%";
    const fx = ledFx.value;
    $("led_freq_label").textContent = fx === "wheel" ? "Скорость колеса" : "Частота";
    $("led_amp_label").textContent = fx === "wheel" ? "Максимальная яркость" : "Амплитуда";
    ledFreq.disabled = fx === "off";
    ledAmp.disabled = fx === "off";
}

function syncEffectTuning(s) {
    if (!ledFxTouched && document.activeElement !== ledFx) {
        ledFx.value = s.led_effect || "off";
    }
    syncLocalTuning();
    if (!ledTuningTouched) {
        if (s.led_freq_pct != null && document.activeElement !== ledFreq) {
            ledFreq.value = String(s.led_freq_pct);
            $("led_freq_out").textContent = ledFreq.value + "%";
        }
        if (s.led_amp_pct != null && document.activeElement !== ledAmp) {
            ledAmp.value = String(s.led_amp_pct);
            $("led_amp_out").textContent = ledAmp.value + "%";
        }
    }
}

ledFx.addEventListener("change", () => { ledFxTouched = true; syncLocalTuning() });
ledFreq.addEventListener("input", () => { ledTuningTouched = true; $("led_freq_out").textContent = ledFreq.value + "%" });
ledAmp.addEventListener("input", () => { ledTuningTouched = true; $("led_amp_out").textContent = ledAmp.value + "%" });
syncLocalTuning();

$("led_fx_apply").onclick = async () => {
    const effect = ledFx.value;
    const payload = { effect };
    if (effect !== "off") {
        payload.freq_pct = parseInt(ledFreq.value, 10);
        payload.amp_pct = parseInt(ledAmp.value, 10);
    }
    try {
        const r = await fetch("/api/led/effect", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(payload) });
        const k = await r.json();
        ledFxTouched = false;
        ledTuningTouched = false;
        poll();
    } catch (e) { alert("ошибка: " + e) }
};
