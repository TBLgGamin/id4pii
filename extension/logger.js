(() => {
  const STORAGE_KEY = "id4pii.debug";
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

  if (typeof globalThis !== "undefined") {
    globalThis.__id4pii_log = api;
  }

  try {
    chrome.storage.local.get(STORAGE_KEY, (r) => {
      if (chrome.runtime.lastError) return;
      api.setLevel(r && r[STORAGE_KEY]);
    });
    chrome.storage.onChanged.addListener((changes, area) => {
      if (area === "local" && changes[STORAGE_KEY]) {
        api.setLevel(!!changes[STORAGE_KEY].newValue);
      }
    });
  } catch (_) {}

  if (typeof self !== "undefined") {
    self.id4pii = self.id4pii || {};
    self.id4pii.debug = (on) => {
      try {
        chrome.storage.local.set({ [STORAGE_KEY]: !!on });
      } catch (_) {}
      api.setLevel(!!on);
      return !!on;
    };
  }
})();
