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
        $("gw").textContent = s.gateway || "нет (DHCP-стек не поднят)";
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
