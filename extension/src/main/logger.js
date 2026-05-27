(() => {
  const TAG = "[id4pii]";
  let LEVEL = "off";

  function fmt(component, event, fields) {
    if (!fields) return `${TAG}[${component}] ${event}`;
    const pairs = Object.entries(fields)
      .map(([k, v]) => `${k}=${safeJson(v)}`)
      .join(" ");
    return `${TAG}[${component}] ${event}${pairs ? " " + pairs : ""}`;
  }
  function safeJson(v) {
    try { return JSON.stringify(v); } catch { return String(v); }
  }

  const api = {
    debug(component, event, fields) {
      if (LEVEL === "debug") console.log(fmt(component, event, fields));
    },
    info(component, event, fields) {
      console.log(fmt(component, event, fields));
    },
    warn(component, event, fields) {
      console.warn(fmt(component, event, fields));
    },
    error(component, event, fields) {
      console.error(fmt(component, event, fields));
    },
    newReqId() {
      try {
        return crypto.randomUUID().replace(/-/g, "").slice(0, 8);
      } catch {
        return Math.floor(Math.random() * 0xffffffff).toString(16).padStart(8, "0");
      }
    },
    setLevel(on) {
      LEVEL = on ? "debug" : "off";
    },
    isDebug() {
      return LEVEL === "debug";
    },
  };

  window.__id4pii_log = api;

  document.addEventListener("id4pii-iso2main", (ev) => {
    let msg;
    try { msg = JSON.parse(ev.detail); } catch { return; }
    if (msg && msg.type === "debug-set") api.setLevel(!!msg.on);
  });
})();
