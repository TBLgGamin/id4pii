(() => {
  const LOG = window.__id4pii_log || {
    debug(){}, info(){}, warn(){}, error(){}, isDebug(){return false;}, newReqId(){return"";}
  };
  const ns = window.id4pii || {};
  let vaultEntries = [];

  LOG.debug("iso", "boot", { host: location.hostname });

  function sendToPage(payload) {
    document.dispatchEvent(new CustomEvent("id4pii-iso2main", { detail: JSON.stringify(payload) }));
  }

  function broadcastVault(reason) {
    LOG.debug("iso", "msg-to-main", { type: "vault", reason, size: vaultEntries.length });
    sendToPage({ type: "vault", entries: vaultEntries });
  }

  function normalize(entries) {
    const arr = Array.isArray(entries) ? entries : [];
    return arr
      .map((e) => {
        if (Array.isArray(e) && e.length === 2) return { fake: e[0], real: e[1] };
        if (e && typeof e === "object" && typeof e.fake === "string") return { fake: e.fake, real: e.real };
        return null;
      })
      .filter((e) => e && e.fake);
  }

  function updateVault(entries, reason) {
    const next = normalize(entries);
    const before = vaultEntries.length;
    if (next.length === 0 && before > 0) {
      LOG.debug("iso", "vault-update", { sizeIncoming: 0, sizeBefore: before, sizeAfter: before, decision: "suppress", reason });
      return;
    }
    vaultEntries = next;
    LOG.debug("iso", "vault-update", { sizeIncoming: next.length, sizeBefore: before, sizeAfter: vaultEntries.length, decision: "accept", reason });
    broadcastVault(reason);
  }

  function pushDebugFlagToMain() {
    try {
      chrome.storage.local.get("id4pii.debug", (r) => {
        if (chrome.runtime.lastError) return;
        sendToPage({ type: "debug-set", on: !!(r && r["id4pii.debug"]) });
      });
    } catch (_) {}
  }

  chrome.storage.onChanged.addListener((changes, area) => {
    if (area === "local" && changes["id4pii.debug"]) {
      sendToPage({ type: "debug-set", on: !!changes["id4pii.debug"].newValue });
    }
  });

  chrome.runtime.onMessage.addListener((msg) => {
    if (!msg || typeof msg.type !== "string") return;
    LOG.debug("iso", "msg-from-bg", { type: msg.type, size: msg.entries ? msg.entries.length : undefined });
    if (msg.type === "vault") updateVault(msg.entries, "bg-push");
    else if (msg.type === "vault_cleared") {
      const before = vaultEntries.length;
      vaultEntries = [];
      LOG.info("iso", "vault-cleared", { sizeBefore: before });
      sendToPage({ type: "vault", entries: vaultEntries });
    }
  });

  function requestVault() {
    try {
      chrome.runtime.sendMessage({ type: "vault_get" }, (resp) => {
        if (chrome.runtime.lastError) return;
        const entries = resp && resp.entries;
        LOG.debug("iso", "vault-get-reply", { size: entries ? entries.length : 0 });
        if (entries && entries.length > 0) updateVault(entries, "vault-get");
      });
    } catch (_) {}
  }

  document.addEventListener("id4pii-main2iso", (ev) => {
    let msg;
    try { msg = JSON.parse(ev.detail); } catch { return; }
    if (!msg || typeof msg.type !== "string") return;
    LOG.debug("iso", "msg-from-main", { type: msg.type, reqId: msg.id });
    if (msg.type === "anonymize-request") {
      const id = msg.id;
      try {
        chrome.runtime.sendMessage({ type: "anonymize", text: msg.text || "", reqId: id }, (resp) => {
          if (chrome.runtime.lastError) {
            sendToPage({ type: "anonymize-reply", id, error: chrome.runtime.lastError.message || "runtime error" });
            return;
          }
          if (resp && resp.ok && resp.reply && resp.reply.type === "anonymized") {
            sendToPage({ type: "anonymize-reply", id, text: resp.reply.text });
          } else if (resp && resp.ok && resp.reply && resp.reply.type === "no_change") {
            sendToPage({ type: "anonymize-reply", id, error: "no_change:" + (resp.reply.reason || "") });
          } else {
            sendToPage({ type: "anonymize-reply", id, error: (resp && resp.error) || "no_reply" });
          }
        });
      } catch (e) {
        sendToPage({ type: "anonymize-reply", id, error: String(e) });
      }
    } else if (msg.type === "ready") {
      pushDebugFlagToMain();
      broadcastVault("ready");
      requestVault();
    } else if (msg.type === "show-overlay") {
      if (ns.ui && ns.ui.show) ns.ui.show(msg.kind || "anonymize", msg.rect || null);
    }
  });

  pushDebugFlagToMain();
  requestVault();
})();
