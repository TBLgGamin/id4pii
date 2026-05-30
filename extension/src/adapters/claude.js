(() => {
  const api = window.__id4pii_main;
  if (!api) return;

  const EXTRACTED_CONTENT_KEY = "extracted_content";

  async function anonymizeValue(core, reqId, value) {
    if (typeof value !== "string") return null;
    if (value.length < core.constants.MIN_INTERESTING_BODY) return null;
    if (core.helpers.containsKnownSurrogate(value)) return null;
    try {
      const out = await core.helpers.callAnonymize(reqId, value);
      return typeof out === "string" && out !== value ? out : null;
    } catch (err) {
      if (typeof err === "string" && err.startsWith("no_change:")) return null;
      throw err;
    }
  }

  function lastKey(path) {
    const k = path[path.length - 1];
    return typeof k === "string" ? k : "";
  }

  function collectCandidates(core, parsed) {
    const strings = [];
    core.helpers.walkStringPaths(parsed, [], strings);
    const out = [];
    for (const s of strings) {
      if (lastKey(s.path) === EXTRACTED_CONTENT_KEY) {
        out.push(s);
      } else if (core.helpers.isUserContentPath(s.path) && core.helpers.looksLikeProse(s.value)) {
        out.push(s);
      }
    }
    return out;
  }

  async function anonymizeJsonBody(core, reqId, text) {
    if (!text || text.length < core.constants.MIN_INTERESTING_BODY) return null;
    let parsed;
    try {
      parsed = JSON.parse(text);
    } catch {
      if (core.helpers.looksLikeProse(text) && !core.helpers.containsKnownSurrogate(text)) {
        return anonymizeValue(core, `${reqId}-raw`, text).catch((err) => {
          core.log.warn("main", "claude-anonymize-error", { reqId, error: String(err) });
          return null;
        });
      }
      return null;
    }

    const candidates = collectCandidates(core, parsed);
    if (candidates.length === 0) return null;

    let changed = false;
    let idx = 0;
    for (const c of candidates) {
      const fieldReqId = `${reqId}-${idx++}`;
      const isFile = lastKey(c.path) === EXTRACTED_CONTENT_KEY;
      try {
        const out = await anonymizeValue(core, fieldReqId, c.value);
        core.log.debug("main", "claude-field", {
          reqId,
          pathTail: lastKey(c.path),
          kind: isFile ? "attachment" : "message",
          lenBefore: c.value.length,
          changed: out != null,
        });
        if (out != null) {
          core.helpers.setAtPath(parsed, c.path, out);
          changed = true;
        }
      } catch (err) {
        core.log[isFile ? "warn" : "debug"]("main", "claude-anonymize-error", { reqId, kind: isFile ? "attachment" : "message", error: String(err) });
      }
    }
    return changed ? JSON.stringify(parsed) : null;
  }

  api.registerAdapter({
    name: "claude",
    hosts: [/(^|\.)claude\.ai$/i],
    chatPatterns: [
      /\/chat_conversations\/[^/]+\/completion/,
      /\/chat_conversations\/[^/]+\/retry_completion/,
      /\/api\/append_message/,
      /\/wiggle\/upload-file/,
      /\/convert_document/,
      /\/v1\/messages/,
    ],
    wrapsResponse: true,
    async anonymizeBody(core, reqId, rawBody) {
      if (rawBody instanceof FormData) {
        const out = await core.helpers.anonymizeUpload(reqId, rawBody);
        return out == null ? rawBody : out;
      }
      const text = await core.helpers.bodyAsString(rawBody);
      if (text == null) return rawBody;
      const safe = await anonymizeJsonBody(core, reqId, text);
      return safe == null ? rawBody : safe;
    },
  });
})();
