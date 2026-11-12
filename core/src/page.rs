//! SPA-страница настроек (инлайн HTML+JS, без внешних библиотек).
//! Хост-тестируется: проверяем целостность константы и ключевые элементы.

pub const PAGE_HTML: &str = r#"<!DOCTYPE html>
<html lang="ru"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>NixieClock ESP32</title>
<style>
:root{--bg:#12100d;--fg:#e8dcc4;--acc:#ff9d45;--dim:#8a7f6a;--card:#1d1a15;--ok:#7ac74c;--warn:#ffcc66;--err:#ff6b6b}
*{box-sizing:border-box}body{background:var(--bg);color:var(--fg);font:15px/1.45 system-ui,sans-serif;margin:0;padding:16px;max-width:860px;margin-inline:auto}
h1{font-size:20px;color:var(--acc);margin:0 0 12px}h2{font-size:14px;color:var(--dim);text-transform:uppercase;letter-spacing:.08em;margin:18px 0 6px}
.card{background:var(--card);border:1px solid #2c2820;border-radius:10px;padding:10px 14px}
.kv{display:grid;grid-template-columns:14ch 1fr;gap:2px 10px}
.kv b{color:var(--dim);font-weight:600}
button{background:var(--acc);color:#191510;border:0;border-radius:8px;padding:8px 14px;font-weight:700;cursor:pointer}
button.sec{background:#2c2820;color:var(--fg)}
input[type=text]{width:100%;background:#0d0b08;color:var(--fg);border:1px solid #2c2820;border-radius:8px;padding:8px}
pre{white-space:pre-wrap;word-break:break-word;max-height:220px;overflow:auto;background:#0d0b08;border-radius:8px;padding:8px;font-size:12.5px}
.row{display:flex;gap:8px;margin-top:8px;flex-wrap:wrap}
#ota_state.ok{color:var(--ok)}#ota_state.warn{color:var(--warn)}#ota_state.err{color:var(--err)}
.note{color:var(--dim);font-size:13px}
</style></head><body>
<h1>⚡ NixieClock ESP32</h1>

<h2>Статус</h2><div class="card"><div class="kv">
<b>uptime</b><span id="uptime">—</span>
<b>firmware</b><span id="fw">—</span>
<b>IP</b><span id="ip">—</span>
<b>OTA-состояние</b><span id="ota_state">—</span>
<b>лог-коллектор</b><span id="log_url" class="note">—</span>
</div></div>

<h2>Wi-Fi</h2><div class="card"><span class="note">выбор сети и пароль — в модуле M3 (NVS); сейчас точка доступа зашита в прошивку.</span></div>

<h2>Время</h2><div class="card"><span class="note">DS1307 + NTP + часовой пояс — модуль M4.</span></div>

<h2>Дисплей</h2><div class="card"><span class="note">яркость/режимы индикации — модуль M5.</span></div>

<h2>Датчики</h2><div class="card"><span class="note">CO2 / температура / напряжения — модуль M6.</span></div>

<h2>Обновление (OTA)</h2><div class="card">
<input type="text" id="ota_url" value="http://192.168.0.28:8000/app_ota.bin">
<div class="row"><button id="ota_btn">Обновить с URL</button><button class="sec" id="revert_btn">Откатить образ</button></div>
<div id="ota_msg" class="note"></div>
</div>

<h2>Логи устройства</h2><div class="card"><pre id="logs">(нажмите «Загрузить»)</pre>
<div class="row"><button class="sec" id="log_btn">Загрузить</button></div></div>

<script>
"use strict";
const $=id=>document.getElementById(id);
const fmtU=s=>{const d=Math.floor(s/86400),h=Math.floor(s%86400/3600),m=Math.floor(s%3600/60);return (d?d+"д ":"")+h+"ч "+m+"м "+(s%60)+"с"};
async function j(u){const r=await fetch(u,{cache:"no-store"});return r.json()}
async function poll(){
 try{
  const s=await j("/api/status");
  $("uptime").textContent=fmtU(s.uptime);
  $("fw").textContent=s.fw+" v"+s.version;
  $("ip").textContent=s.ip||"нет IP (wifi не подключён)";
  const el=$("ota_state");el.textContent=s.ota_state+(s.pending?" (ждет подтверждения)":"");
  el.className=s.ota_state==="Valid"?"ok":(s.pending||s.ota_state==="New"?"warn":"err");
 }catch(e){$("uptime").textContent="сервер недоступен"}
}
poll();setInterval(poll,3000);
$("ota_btn").onclick=async()=>{
 const u=$("ota_url").value.trim();if(!u)return;
 $("ota_msg").textContent="запрошено…";
 try{const r=await fetch("/ota?url="+encodeURIComponent(u));$("ota_msg").textContent=await r.text()}catch(e){$("ota_msg").textContent="ошибка: "+e}
};
$("revert_btn").onclick=async()=>{
 if(!confirm("Пометить текущий образ INVALID и перезагрузиться (откат на старый слот)?"))return;
 try{const r=await fetch("/revert");$("ota_msg").textContent=await r.text()}catch(e){$("ota_msg").textContent="сброс выполнен, ждите перезагрузки"}
};
$("log_btn").onclick=async()=>{
 try{const r=await fetch("/logs",{cache:"no-store"});$("logs").textContent=await r.text()}catch(e){$("logs").textContent="ошибка: "+e}
};
</script></body></html>
"#;

#[cfg(test)]
mod tests {
    use alloc::format;

    use super::PAGE_HTML;

    #[test]
    fn page_is_coherent_html() {
        assert!(PAGE_HTML.starts_with("<!DOCTYPE html>"));
        assert!(PAGE_HTML.trim_end().ends_with("</html>"));
        for id in ["uptime", "fw", "ip", "ota_state", "ota_url", "ota_btn", "revert_btn", "logs"] {
            assert!(PAGE_HTML.contains(&format!("id=\"{id}\"")), "missing #{id}");
        }
        // SPA ходит только на наши эндпоинты
        for ep in ["/api/status", "/ota?url=", "/revert", "/logs"] {
            assert!(PAGE_HTML.contains(ep), "missing endpoint {ep}");
        }
        assert!(!PAGE_HTML.contains("http://localhost"), "no dev leftovers");
        // относительные fetch -> страница работает с любого адреса платы
        assert!(PAGE_HTML.contains("j(\"/api/status\")"));
        assert!(PAGE_HTML.contains("fetch(\"/logs\""));
    }

    #[test]
    fn page_fits_budget() {
        // держим страницу компактной: ро флеш-секция + быстрый первый рендер
        assert!(PAGE_HTML.len() < 6000, "SPA page too large: {}", PAGE_HTML.len());
    }
}
