(() => {
  const LOG = window.__id4pii_log || {
    debug(){}, info(){}, warn(){}, error(){}, isDebug(){return false;}, newReqId(){return String(Math.random()).slice(2,10);}
  };
  LOG.info("main", "boot", { host: location.hostname });

  const CHAT_PATTERNS = [
    /\/backend-api\/conversation/,
    /\/backend-anon\/conversation/,
    /\/api\/organizations\/[^/]+\/chat_conversations\/[^/]+\/completion/,
    /\/api\/organizations\/[^/]+\/chat_conversations\/[^/]+\/retry_completion/,
    /\/api\/append_message/,
    /\/_\/BardChatUi\/data\/.*GenerateContent/i,
    /\/_\/BardChatUi\/data\/.*StreamGenerate/i,
    /\/v1beta\/models\/.*:streamGenerateContent/,
    /\/v1\/messages/,
    /\/v1\/chat\/completions/,
  ];

  const SURROGATE_BUFFER = 80;
  const REQUEST_TIMEOUT_MS = 30000;
  const MIN_INTERESTING_BODY = 8;
  const RESPONSE_CHUNK_LOG_SAMPLE = 8;

  let vaultPairs = [];
  const pending = new Map();

  function sendToIsolated(payload) {
    document.dispatchEvent(new CustomEvent("id4pii-main2iso", { detail: JSON.stringify(payload) }));
  }

  document.addEventListener("id4pii-iso2main", (ev) => {
    let msg;
    try { msg = JSON.parse(ev.detail); } catch { return; }
    if (!msg || typeof msg.type !== "string") return;
    if (msg.type === "vault") {
      const entries = msg.entries || [];
      vaultPairs = entries
        .filter((e) => e && e.fake)
        .map((e) => [e.fake, e.real])
        .sort((a, b) => b[0].length - a[0].length);
      LOG.debug("main", "vault-update", { size: vaultPairs.length });
    } else if (msg.type === "anonymize-reply") {
      const p = pending.get(msg.id);
      if (!p) return;
      pending.delete(msg.id);
      clearTimeout(p.timer);
      if (msg.error) p.reject(msg.error);
      else p.resolve(msg.text);
    }
  });

  function callAnonymize(reqId, text) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        if (pending.has(reqId)) { pending.delete(reqId); reject("timeout"); }
      }, REQUEST_TIMEOUT_MS);
      pending.set(reqId, { resolve, reject, timer });
      sendToIsolated({ type: "anonymize-request", id: reqId, text });
    });
  }

  function restoreString(s) {
    if (!s || vaultPairs.length === 0) return { out: s, count: 0 };
    let out = s;
    let count = 0;
    for (const [fake, real] of vaultPairs) {
      if (out.indexOf(fake) !== -1) {
        const parts = out.split(fake);
        count += parts.length - 1;
        out = parts.join(real);
      }
    }
    return { out, count };
  }

  function urlOf(input) {
    if (!input) return "";
    if (typeof input === "string") return input;
    if (input instanceof URL) return input.href;
    if (input && typeof input.url === "string") return input.url;
    return String(input);
  }

  function urlPathTail(url) {
    try {
      const u = url.startsWith("http") ? new URL(url) : new URL(url, location.origin);
      return u.pathname.slice(-60);
    } catch {
      return String(url).slice(-60);
    }
  }

  function isChatRequest(url, method) {
    if (!url) return false;
    if (method && !/^(POST|PUT|PATCH)$/i.test(method)) return false;
    return CHAT_PATTERNS.some((re) => re.test(url));
  }

  async function bodyAsString(body) {
    if (body == null) return null;
    if (typeof body === "string") return body;
    if (body instanceof ArrayBuffer) {
      try { return new TextDecoder().decode(body); } catch { return null; }
    }
    if (ArrayBuffer.isView(body)) {
      try { return new TextDecoder().decode(body.buffer); } catch { return null; }
    }
    if (body instanceof Blob) {
      try { return await body.text(); } catch { return null; }
    }
    if (body instanceof URLSearchParams) return body.toString();
    if (body instanceof FormData) return null;
    if (body instanceof ReadableStream) return null;
    return null;
  }

  let cursorX = -1;
  let cursorY = -1;
  for (const root of [window, document]) {
    root.addEventListener("pointermove", (e) => {
      cursorX = e.clientX;
      cursorY = e.clientY;
    }, true);
    root.addEventListener("mousemove", (e) => {
      cursorX = e.clientX;
      cursorY = e.clientY;
    }, true);
  }

  function cursorAnchor() {
    if (cursorX < 0 || cursorY < 0) return null;
    return { left: cursorX, top: cursorY, width: 0, height: 0, cursor: true };
  }

  function containsKnownSurrogate(s) {
    for (let i = 0; i < vaultPairs.length; i++) {
      if (s.indexOf(vaultPairs[i][0]) !== -1) return true;
    }
    return false;
  }

  const USER_CONTENT_KEYS = new Set([
    "text", "content", "input", "prompt", "message", "msg", "body", "value",
  ]);
  const USER_CONTENT_PARENTS = new Set([
    "messages", "contents", "parts", "prompts", "completion",
  ]);
  const MAX_CANDIDATES_PER_REQUEST = 4;

  function looksLikeProse(s) {
    if (s.length < 12) return false;
    if (!/\s/.test(s)) return false;
    if (/^[A-Za-z0-9_\-./:]+$/.test(s)) return false;
    if (/^[0-9a-f-]{8,}$/i.test(s)) return false;
    return true;
  }

  function isUserContentPath(path) {
    if (path.length === 0) return false;
    const last = path[path.length - 1];
    if (typeof last === "string" && USER_CONTENT_KEYS.has(last.toLowerCase())) return true;
    for (const seg of path) {
      if (typeof seg === "string" && USER_CONTENT_PARENTS.has(seg.toLowerCase())) return true;
    }
    return false;
  }

  function walkStringPaths(obj, path, out) {
    if (typeof obj === "string") {
      out.push({ path: path.slice(), value: obj });
    } else if (Array.isArray(obj)) {
      for (let i = 0; i < obj.length; i++) {
        path.push(i);
        walkStringPaths(obj[i], path, out);
        path.pop();
      }
    } else if (obj && typeof obj === "object") {
      for (const k of Object.keys(obj)) {
        path.push(k);
        walkStringPaths(obj[k], path, out);
        path.pop();
      }
    }
  }

  function setAtPath(root, path, value) {
    let cur = root;
    for (let i = 0; i < path.length - 1; i++) cur = cur[path[i]];
    cur[path[path.length - 1]] = value;
  }

  async function anonymizeBodyString(reqId, text) {
    if (!text || text.length < MIN_INTERESTING_BODY) return text;
    let parsed;
    try {
      parsed = JSON.parse(text);
    } catch {
      if (looksLikeProse(text) && !containsKnownSurrogate(text)) {
        try {
          const out = await callAnonymize(reqId + "-raw", text);
          return typeof out === "string" ? out : text;
        } catch (err) {
          LOG.warn("main", "fetch-anonymize-error", { reqId, error: String(err) });
        }
      }
      return text;
    }

    const strings = [];
    walkStringPaths(parsed, [], strings);
    const interesting = strings.filter(
      (s) =>
        looksLikeProse(s.value) &&
        !containsKnownSurrogate(s.value) &&
        isUserContentPath(s.path)
    );
    const candidates = interesting.slice(-MAX_CANDIDATES_PER_REQUEST);
    LOG.debug("main", "fetch-fields", {
      reqId,
      totalStrings: strings.length,
      interesting: interesting.length,
      candidates: candidates.length,
    });
    if (candidates.length === 0) return text;
    let changed = false;
    let idx = 0;
    for (const c of candidates) {
      const fieldReqId = `${reqId}-${idx++}`;
      try {
        const out = await callAnonymize(fieldReqId, c.value);
        const did = typeof out === "string" && out !== c.value;
        LOG.debug("main", "fetch-field-anonymized", {
          reqId,
          pathTail: String(c.path[c.path.length - 1]),
          lenBefore: c.value.length,
          lenAfter: did ? out.length : c.value.length,
          changed: did,
        });
        if (did) {
          setAtPath(parsed, c.path, out);
          changed = true;
        }
      } catch (err) {
        if (typeof err === "string" && err.startsWith("no_change:")) continue;
        LOG.warn("main", "fetch-anonymize-error", { reqId, field: String(c.path[c.path.length - 1]), error: String(err) });
      }
    }
    return changed ? JSON.stringify(parsed) : text;
  }

  function shouldWrapResponse(response) {
    if (!response || !response.body) return false;
    const ct = (response.headers.get("content-type") || "").toLowerCase();
    return /^(application\/json|application\/x-ndjson|text\/event-stream|text\/plain)/.test(ct);
  }

  function waitForVault(timeoutMs) {
    return new Promise((resolve) => {
      if (vaultPairs.length > 0) { resolve(0); return; }
      const start = Date.now();
      const tick = () => {
        const elapsed = Date.now() - start;
        if (vaultPairs.length > 0 || elapsed > timeoutMs) resolve(elapsed);
        else setTimeout(tick, 20);
      };
      tick();
    });
  }

  function wrapStreamingResponse(reqId, response) {
    if (!shouldWrapResponse(response)) return response;
    const ct = response.headers.get("content-type") || "";
    LOG.debug("main", "response-wrap", { reqId, contentType: ct.slice(0, 64), vaultSize: vaultPairs.length });
    const decoder = new TextDecoder("utf-8", { fatal: false });
    const encoder = new TextEncoder();
    let buffer = "";
    let totalIn = 0;
    let totalOut = 0;
    let totalRestored = 0;
    let chunkIdx = 0;
    const startedAt = Date.now();
    const transformer = new TransformStream({
      async transform(chunk, controller) {
        totalIn += chunk.byteLength;
        buffer += decoder.decode(chunk, { stream: true });
        if (vaultPairs.length === 0) {
          const waited = await waitForVault(2000);
          if (waited > 0) LOG.debug("main", "response-wait-vault", { reqId, waitedMs: waited });
        }
        const safe = Math.max(0, buffer.length - SURROGATE_BUFFER);
        if (safe > 0) {
          const emit = buffer.slice(0, safe);
          buffer = buffer.slice(safe);
          const { out, count } = restoreString(emit);
          const bytes = encoder.encode(out);
          totalOut += bytes.byteLength;
          totalRestored += count;
          if (count > 0 || chunkIdx % RESPONSE_CHUNK_LOG_SAMPLE === 0) {
            LOG.debug("main", "response-chunk", { reqId, idx: chunkIdx, bytesIn: chunk.byteLength, bytesOut: bytes.byteLength, restored: count });
          }
          chunkIdx++;
          controller.enqueue(bytes);
        }
      },
      async flush(controller) {
        buffer += decoder.decode();
        if (vaultPairs.length === 0) await waitForVault(2000);
        if (buffer) {
          const { out, count } = restoreString(buffer);
          const bytes = encoder.encode(out);
          totalOut += bytes.byteLength;
          totalRestored += count;
          controller.enqueue(bytes);
          buffer = "";
        }
        LOG.debug("main", "response-end", { reqId, totalBytesIn: totalIn, totalBytesOut: totalOut, totalRestored, durationMs: Date.now() - startedAt });
      },
    });
    const headers = new Headers(response.headers);
    headers.delete("content-length");
    return new Response(response.body.pipeThrough(transformer), {
      status: response.status,
      statusText: response.statusText,
      headers,
    });
  }

  const origFetch = window.fetch.bind(window);

  window.fetch = async function (input, init) {
    let url = "";
    let method = "GET";
    let body = null;
    if (input instanceof Request) {
      url = input.url;
      method = input.method;
      if (!init || init.body == null) {
        try { body = await input.clone().text(); } catch { body = null; }
      }
    } else {
      url = urlOf(input);
    }
    if (init) {
      if (init.method) method = init.method;
      if (init.body != null) body = await bodyAsString(init.body);
    }

    const chat = isChatRequest(url, method);
    const reqId = LOG.newReqId();
    LOG.debug("main", "fetch-intercept", { reqId, urlPathTail: urlPathTail(url), method, isChat: chat, hasBody: body != null });

    if (chat && body != null) {
      const safe = await anonymizeBodyString(reqId, body);
      if (safe !== body) {
        LOG.debug("main", "fetch-body-out", { reqId, sizeBefore: body.length, sizeAfter: safe.length, changed: true });
        sendToIsolated({ type: "show-overlay", kind: "anonymize", rect: cursorAnchor() });
        if (init) {
          init = { ...init, body: safe };
        } else if (input instanceof Request) {
          input = new Request(input, { body: safe });
        } else {
          init = { method, body: safe };
        }
      } else {
        LOG.debug("main", "fetch-body-out", { reqId, sizeBefore: body.length, sizeAfter: safe.length, changed: false });
      }
    }

    const response = await origFetch(input, init);
    return wrapStreamingResponse(reqId, response);
  };

  const OrigXHR = window.XMLHttpRequest;
  function PatchedXHR() {
    const xhr = new OrigXHR();
    let url = "";
    let method = "GET";
    const origOpen = xhr.open.bind(xhr);
    const origSend = xhr.send.bind(xhr);
    xhr.open = function (m, u, ...rest) {
      method = m;
      url = u;
      return origOpen(m, u, ...rest);
    };
    xhr.send = function (bodyArg) {
      if (!isChatRequest(url, method)) {
        return origSend(bodyArg);
      }
      const reqId = LOG.newReqId();
      LOG.debug("main", "xhr-intercept", { reqId, urlPathTail: urlPathTail(url), method });
      (async () => {
        const text = await bodyAsString(bodyArg);
        if (text == null) {
          return origSend(bodyArg);
        }
        const safe = await anonymizeBodyString(reqId, text);
        if (safe !== text) {
          const anchor = findInputForOverlay();
          sendToIsolated({ type: "show-overlay", kind: "anonymize", rect: rectOf(anchor) });
        }
        origSend(safe);
      })().catch((err) => {
        LOG.warn("main", "xhr-intercept-error", { reqId, error: String(err) });
        origSend(bodyArg);
      });
    };
    return xhr;
  }
  PatchedXHR.prototype = OrigXHR.prototype;
  Object.setPrototypeOf(PatchedXHR, OrigXHR);
  window.XMLHttpRequest = PatchedXHR;

  LOG.info("main", "patched");
  sendToIsolated({ type: "ready" });
})();
