try { importScripts("logger.js"); } catch (_) {}
const LOG = (self.__id4pii_log) || {
  debug() {}, info(){}, warn(){}, error(){}, newReqId(){ return String(Date.now()); }, isDebug(){ return false; }
};

const BRIDGE_URL = "ws://127.0.0.1:7878/ws";
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 30000;
const REQUEST_TIMEOUT_MS = 20000;

let socket = null;
let backoff = RECONNECT_BASE_MS;
let reconnectTimer = null;
const vault = new Map();
const pending = new Map();

LOG.info("bg", "boot");

function setBadge(state) {
  const text = state === "connected" ? "" : "!";
  const color = state === "connected" ? "#2EB88A" : "#B83A2E";
  chrome.action.setBadgeText({ text }).catch(() => {});
  chrome.action.setBadgeBackgroundColor({ color }).catch(() => {});
}

function scheduleReconnect() {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connect();
  }, backoff);
  backoff = Math.min(backoff * 2, RECONNECT_MAX_MS);
}

function connect() {
  LOG.debug("bg", "connect-attempt", { backoff });
  try {
    socket = new WebSocket(BRIDGE_URL);
  } catch (err) {
    LOG.warn("bg", "connect-throw", { error: String(err) });
    setBadge("disconnected");
    scheduleReconnect();
    return;
  }
  socket.addEventListener("open", () => {
    backoff = RECONNECT_BASE_MS;
    setBadge("connected");
    LOG.info("bg", "connect-open");
    safeSend({ type: "hello", host: "extension", tab_id: "" });
  });
  socket.addEventListener("close", () => {
    setBadge("disconnected");
    LOG.info("bg", "connect-close", { pending: pending.size });
    rejectAllPending("bridge disconnected");
    scheduleReconnect();
  });
  socket.addEventListener("error", (e) => {
    LOG.debug("bg", "connect-error", { type: e && e.type });
    setBadge("disconnected");
  });
  socket.addEventListener("message", (ev) => handleSocketMessage(ev.data));
}

function safeSend(obj) {
  if (!socket || socket.readyState !== WebSocket.OPEN) return false;
  try {
    socket.send(JSON.stringify(obj));
    return true;
  } catch (err) {
    LOG.warn("bg", "send-throw", { error: String(err) });
    return false;
  }
}

function rejectAllPending(reason) {
  for (const entry of pending.values()) {
    clearTimeout(entry.timeout);
    entry.reject(reason);
  }
  pending.clear();
}

function handleSocketMessage(raw) {
  const lines = String(raw).split("\n").map((s) => s.trim()).filter(Boolean);
  for (const line of lines) {
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      LOG.warn("bg", "ws-parse-fail", { len: line.length });
      continue;
    }
    routeServerMessage(msg);
  }
}

function routeServerMessage(msg) {
  switch (msg.type) {
    case "vault": {
      const entries = msg.entries || [];
      const before = vault.size;
      if (entries.length === 0 && before > 0) {
        LOG.warn("bg", "vault-empty-suppressed", { localSize: before });
        break;
      }
      vault.clear();
      for (const entry of entries) {
        if (entry && entry.fake) {
          vault.set(entry.fake, { real: entry.real, category: entry.category || "" });
        }
      }
      LOG.debug("bg", "vault-recv", { source: "snapshot", sizeBefore: before, sizeAfter: vault.size });
      broadcastVault("snapshot");
      break;
    }
    case "vault_cleared": {
      const before = vault.size;
      vault.clear();
      LOG.info("bg", "vault-cleared", { sizeBefore: before, removed: msg.removed });
      chrome.tabs.query({}, (tabs) => {
        for (const tab of tabs) {
          if (!tab.id) continue;
          chrome.tabs.sendMessage(tab.id, { type: "vault_cleared" }).catch(() => {});
        }
      });
      break;
    }
    case "vault_delta": {
      const before = vault.size;
      const added = msg.added || [];
      for (const pair of added) {
        if (Array.isArray(pair) && pair.length === 2) {
          vault.set(pair[0], { real: pair[1], category: "" });
        }
      }
      LOG.debug("bg", "vault-recv", { source: "delta", sizeBefore: before, sizeAfter: vault.size, deltaCount: added.length });
      broadcastVault("delta");
      break;
    }
    case "anonymized":
    case "restored":
    case "no_change":
    case "error": {
      const entry = pending.get(msg.id);
      LOG.debug("bg", "ws-reply", { reqId: msg.id, type: msg.type, matched: !!entry });
      if (!entry) return;
      pending.delete(msg.id);
      clearTimeout(entry.timeout);
      if (msg.type === "error") entry.reject(msg.message || "bridge error");
      else entry.resolve(msg);
      break;
    }
    case "hello_ack":
      LOG.debug("bg", "hello-ack", { clientId: msg.client_id });
      break;
    case "pong":
      LOG.debug("bg", "pong");
      break;
    case "event":
      LOG.debug("bg", "ws-event", { kind: msg.kind, op: msg.op });
      break;
    default:
      LOG.debug("bg", "ws-unknown-type", { type: msg.type });
      break;
  }
}

function broadcastVault(source) {
  const entries = Array.from(vault.entries()).map(([fake, v]) => [fake, v.real]);
  chrome.tabs.query({}, (tabs) => {
    const targets = tabs.filter((t) => t.id);
    LOG.debug("bg", "vault-broadcast", { source, size: entries.length, tabCount: targets.length });
    for (const tab of targets) {
      chrome.tabs.sendMessage(tab.id, { type: "vault", entries }).catch(() => {});
    }
  });
}

function callBridge(type, text, reqId) {
  return new Promise((resolve, reject) => {
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      LOG.warn("bg", "req-reject-disconnected", { reqId, type });
      reject("bridge disconnected");
      return;
    }
    const id = reqId || LOG.newReqId();
    const timeout = setTimeout(() => {
      if (pending.has(id)) {
        pending.delete(id);
        LOG.warn("bg", "req-timeout", { reqId: id, type });
        reject("timeout");
      }
    }, REQUEST_TIMEOUT_MS);
    pending.set(id, { resolve, reject, timeout });
    LOG.debug("bg", "req-send", { reqId: id, type, textLen: (text || "").length });
    const sent = safeSend({ type, id, text });
    if (!sent) {
      pending.delete(id);
      clearTimeout(timeout);
      LOG.warn("bg", "req-send-failed", { reqId: id, type });
      reject("send failed");
    }
  });
}

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (!msg || typeof msg.type !== "string") return false;
  LOG.debug("bg", "tab-msg-recv", { tabId: sender && sender.tab && sender.tab.id, type: msg.type, reqId: msg.reqId });
  if (msg.type === "anonymize" || msg.type === "restore") {
    callBridge(msg.type, msg.text || "", msg.reqId)
      .then((reply) => sendResponse({ ok: true, reply }))
      .catch((err) => sendResponse({ ok: false, error: String(err) }));
    return true;
  }
  if (msg.type === "vault_get") {
    const entries = Array.from(vault.entries()).map(([fake, v]) => [fake, v.real]);
    LOG.debug("bg", "vault-get-reply", { size: entries.length });
    sendResponse({ entries });
    return false;
  }
  if (msg.type === "status") {
    sendResponse({ connected: socket && socket.readyState === WebSocket.OPEN, vaultSize: vault.size });
    return false;
  }
  return false;
});

self.id4pii = self.id4pii || {};
self.id4pii.clearVault = () => {
  const id = LOG.newReqId();
  const ok = safeSend({ type: "vault_clear", id });
  LOG.info("bg", "vault-clear-requested", { reqId: id, sent: ok });
  if (!ok) {
    console.warn("[id4pii] clearVault: bridge not connected");
    return false;
  }
  console.log("[id4pii] clearVault requested; broadcast will follow");
  return true;
};

setBadge("disconnected");
connect();

setInterval(() => {
  if (socket && socket.readyState === WebSocket.OPEN) {
    LOG.debug("bg", "ping");
    safeSend({ type: "ping" });
  }
}, 20000);
