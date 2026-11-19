"use strict";
const $ = id => document.getElementById(id);
const fmtU = s => { const d = Math.floor(s / 86400), h = Math.floor(s % 86400 / 3600), m = Math.floor(s % 3600 / 60); return (d ? d + "д " : "") + h + "ч " + m + "м " + (s % 60) + "с" };
async function j(u) { const r = await fetch(u, { cache: "no-store" }); return r.json() }
async function poll() {
    try {
        const s = await j("/api/status");
        $("uptime").textContent = fmtU(s.uptime);
        $("fw").textContent = s.fw + " v" + s.version;
        $("ip").textContent = s.ip || "нет IP (wifi не подключён)";
        $("gw").textContent = s.gateway || (s.dhcp_done ? "нет (роутер не передаёт шлюз)" : "нет (DHCP не завершён)");
        $("dns").textContent = s.dns || (s.dhcp_done ? "нет (роутер не передаёт DNS)" : "нет (DHCP не завершён)");
        $("sensor_temp").textContent = s.temp_mc == null ? "нет данных" : (s.temp_mc / 1000).toFixed(1) + " °C";
        $("sensor_co2").textContent = s.co2_ppm == null ? "нет данных" : s.co2_ppm + " ppm";
        $("sensor_v33").textContent = s.v33_mv == null ? "нет данных" : (s.v33_mv / 1000).toFixed(2) + " В";
        $("sensor_v5").textContent = s.v5_mv == null ? "нет данных" : (s.v5_mv / 1000).toFixed(2) + " В";
        $("sensor_v12").textContent = s.v12_mv == null ? "нет данных" : (s.v12_mv / 1000).toFixed(2) + " В";
        $("wifi_cur").textContent = s.wifi_ssid || "—";
        $("wifi_src").textContent = s.wifi_saved ? "из NVS (сохранены)" : "из прошивки (env)";
        const el = $("ota_state"); el.textContent = s.ota_state + (s.pending ? " (ждет подтверждения)" : "");
        el.className = s.ota_state === "Valid" ? "ok" : (s.pending || s.ota_state === "New" ? "warn" : "err");
        $("time_now").textContent = s.time || "—";
        $("time_src").textContent = (s.time_source || "?") + (s.time_synced ? "" : " (не подтверждено)");
        $("ntp_cur").textContent = s.ntp_server || "пулы по умолчанию";
    } catch (e) { $("uptime").textContent = "сервер недоступен" }
}
poll(); setInterval(poll, 3000);
$("ota_btn").onclick = async () => {
    const u = $("ota_url").value.trim(); if (!u) return;
    $("ota_msg").textContent = "запрошено…";
    try { const r = await fetch("/ota?url=" + encodeURIComponent(u)); $("ota_msg").textContent = await r.text() } catch (e) { $("ota_msg").textContent = "ошибка: " + e }
};
$("revert_btn").onclick = async () => {
    if (!confirm("Пометить текущий образ INVALID и перезагрузиться (откат на старый слот)?")) return;
    try { const r = await fetch("/revert"); $("ota_msg").textContent = await r.text() } catch (e) { $("ota_msg").textContent = "сброс выполнен, ждите перезагрузки" }
};
$("log_btn").onclick = async () => {
    try { const r = await fetch("/logs", { cache: "no-store" }); $("logs").textContent = await r.text() } catch (e) { $("logs").textContent = "ошибка: " + e }
};
$("wifi_btn").onclick = async () => {
    const ssid = $("wifi_ssid_in").value.trim(), pass = $("wifi_pass_in").value;
    $("wifi_msg").textContent = "сохраняю…";
    try {
        const r = await fetch("/api/wifi/connect", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ssid, password: pass }) });
        const j = await r.json();
        $("wifi_msg").textContent = j.status === "saved" ? "сохранено в NVS, перезагрузка…" : "ошибка: " + (j.error || r.status);
    } catch (e) { $("wifi_msg").textContent = "ошибка: " + e }
};

$("time_btn").onclick = async () => {
    const v = $("time_set_in").value;
    if (!v) { $("time_msg").textContent = "выберите дату и время (UTC)"; return; }
    const ep = Math.floor(Date.parse(v + "Z") / 1000); // datetime-local трактуем как UTC
    if (isNaN(ep)) { $("time_msg").textContent = "не читается дата"; return; }
    $("time_msg").textContent = "устанавливаю…";
    try {
        const r = await fetch("/api/time", { method: "POST", body: "epoch=" + ep });
        const j = await r.json();
        $("time_msg").textContent = r.ok ? ("установлено: " + j.time + " UTC") : ("ошибка: " + (r.status));
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

// ---- диагностика индикации, самопроверка, питание ----
async function getText(u) { const r = await fetch(u, { cache: "no-store" }); return await r.text() }
async function kv(u) {
    const t = await getText(u), o = {};
    t.split("\n").forEach(l => { const i = l.indexOf("="); if (i > 0) o[l.slice(0, i)] = l.slice(i + 1) });
    return o;
}
const dMsg = $("d_msg");
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
    dMsg.textContent = "…";
    dMsg.textContent = (await getText(u)).trim();
    pollDiag();
}
$("d_clock").onclick = async () => {
    await getText("/display/test?off=1&nostrobe=0");
    $("nostrobe").checked = false;
    dMsg.textContent = (await getText("/display/on")).trim();
    pollDiag();
};
$("d_off").onclick = async () => { dMsg.textContent = (await getText("/display/off")).trim(); pollDiag() };
$("d_cycle").onclick = async () => { dMsg.textContent = (await getText("/display/test?cycle=1&lamp=6")).trim(); pollDiag() };
$("d_raw").onclick = async () => { dMsg.textContent = (await getText("/display/test?rawcycle=1&lamp=6")).trim(); pollDiag() };
$("fade").onchange = async e => {
    dMsg.textContent = (await getText("/display/fade?on=" + (e.target.checked ? 1 : 0))).trim();
    pollDiag();
};

async function selfTest(list, nostrobe) {
    const out = $("st_out"), msg = $("st_msg");
    out.innerHTML = "";
    msg.textContent = "идёт: " + list.join(", ") + (nostrobe ? " (без строба)" : "") + "…";
    const add = (k, v, cls) => {
        const b = document.createElement("b"), s = document.createElement("span");
        b.textContent = k; s.textContent = v; if (cls) s.className = cls;
        out.append(b, s);
    };
    let bad = 0;
    for (const d of list) {
        let t0 = 0, t1 = null;
        try { t0 = (await j("/api/status")).uptime } catch (e) { }
        await getText("/display/test?digit=" + d + "&lamp=6&nostrobe=" + (nostrobe ? 1 : 0));
        await new Promise(r => setTimeout(r, 2200));
        try { t1 = (await j("/api/status")).uptime } catch (e) { }
        if (t1 === null || t1 < t0) {
            add("цифра " + d, "СБРОС ПЛАТЫ" + (t1 === null ? " (нет ответа)" : ""), "err");
            bad++;
            break;
        }
        const w = await kv("/status").catch(() => ({}));
        const rails = w.win33_n && w.win33_n !== "0"
            ? "3.3 В: avg " + (+w.win33_avg / 1000).toFixed(2) + ", мин " + (+w.win33_min / 1000).toFixed(2) +
              ", ниже 3 В " + w.win33_low + "/" + w.win33_n
            : "окно питания ещё не посчитано";
        add("цифра " + d, "ok · " + rails, "ok");
    }
    await getText("/display/test?off=1&nostrobe=0");
    await getText("/display/on");
    msg.textContent = bad ? "ЕСТЬ СБРОС — смотри reset reason ниже" : "все прошли, часы включены";
    pollDiag();
}
$("st_all").onclick = () => selfTest([1, 2, 3, 4, 5, 6, 7, 8, 9], false);
$("st_bad").onclick = () => selfTest([2, 5, 7], false);
$("st_bus").onclick = () => selfTest([2, 5, 7], true);

const VOLTS = ["2.7", "2.8", "2.9", "3.0", "3.1", "3.2", "3.3", "3.4"];
async function pollDiag() {
    let s;
    try { s = await kv("/status") } catch (e) { return }
    const on = v => v === "true";
    const v = (mv, d) => mv && mv !== "0" ? (+mv / 1000).toFixed(d || 2) + " В" : "нет данных";
    $("p_boot").textContent = "#" + (s.boot_count || "?") + " за всё время";
    $("p_bo").textContent = (s.brownout_count || "0") + " сбросов по питанию";
    $("p_rst").textContent = s.reset_reason || "—";
    $("p_bod").textContent = (on(s.brownout_ena) ? "включён" : "ВЫКЛЮЧЕН") +
        " · порог " + (VOLTS[+s.brownout_thres || 0] || "?") + " В" + (on(s.brownout_det) ? " · DET!" : "");
    $("p_min").textContent = v(s.min_3v3_mv);
    $("p_win").textContent = s.win33_n && s.win33_n !== "0"
        ? "avg " + v(s.win33_avg) + " · мин " + v(s.win33_min) + " · ниже 3 В " + s.win33_low + "/" + s.win33_n
        : "нет (меряется в тест-режиме)";
    $("p_post").textContent = (s.post_ok || "0") + " ok · " + (s.post_timeouts || "0") + " таймаутов";
    // Яркость во время фейда: 128 = 100%, 0 = погашена.
    const lampNames = ["Ч1", "Ч2", "М1", "М2", "С1", "С2"];
    const duties = lampNames.map((n, i) => {
        const d = s["display_duty" + i];
        return n + " " + (d == null ? "?" : Math.round(+d * 100 / 128) + "%");
    }).join(" · ");
    $("p_disp").textContent = "mode " + (s.display_mode || "0") +
        " · лампа " + (s.display_lamp === "6" ? "все" : (s.display_lamp || "?")) +
        (on(s.display_on) ? " · вкл" : " · ВЫКЛ") +
        (on(s.display_fade) ? " · fade" : " · без fade") +
        (on(s.nostrobe) ? " · без строба" : "") +
        " · ярк: " + duties;
    if (document.activeElement !== $("fade")) $("fade").checked = on(s.display_fade);
}
pollDiag();
setInterval(pollDiag, 3000);
