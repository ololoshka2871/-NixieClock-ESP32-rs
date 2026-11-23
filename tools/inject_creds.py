import json, re
c = json.load(open("/tmp/creds103.json"))
p = "firmware/.cargo/config.toml"
t = open(p).read()

def q(v):
    return '"' + v.replace('\\', '\\\\').replace('"', '\\"') + '"'

for k in ("SSID", "PASSWORD", "LOG_URL"):
    t = re.sub(r"(?m)^%s\s*=.*$" % k, "%s = %s" % (k, q(c[k])), t)

# SOFTAP_PASS MUST live inside the [env] section: option_env!("SOFTAP_PASS")
# in main.rs is only fed by cargo's [env]. Appending at EOF used to land it
# under [unstable] -> cargo ignored it -> SoftAP compiled out.
m = re.search(r"(?m)^SOFTAP_PASS\s*=.*$", t)
line = "SOFTAP_PASS = %s" % q(c["SOFTAP_PASS"])
if m:
    t = re.sub(r"(?m)^SOFTAP_PASS\s*=.*$", line, t)
else:
    mm = re.search(r'(?m)^LOG_URL\s*=\s*"[^"]*"\s*$', t)
    if not mm:
        raise SystemExit("inject: no LOG_URL anchor in [env]")
    t = t[: mm.end()] + "\n" + line + t[mm.end():]
open(p, "w").write(t)
# self-check: SOFTAP_PASS must be inside [env], not any other section
env = re.search(r"\[env\](.*?)(?=^\[|\Z)", t, re.S | re.M)
if not env or "SOFTAP_PASS" not in env.group(1):
    raise SystemExit("inject: SOFTAP_PASS not inside [env]")
print("config.toml creds injected (SOFTAP_PASS inside [env])")
