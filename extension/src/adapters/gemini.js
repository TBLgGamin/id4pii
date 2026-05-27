(() => {
  const api = window.__id4pii_main;
  if (!api) return;

  async function anonymizeGeminiBody(core, reqId, rawBody) {
    const LOG = core.log;
    let fReq = null;
    let writeBack = null;
    let bodyKind = "unknown";

    if (typeof rawBody === "string") {
      bodyKind = "string";
      let params;
      try { params = new URLSearchParams(rawBody); } catch { return rawBody; }
      fReq = params.get("f.req");
      writeBack = (v) => { params.set("f.req", v); return params.toString(); };
    } else if (rawBody instanceof URLSearchParams) {
      bodyKind = "urlsearchparams";
      fReq = rawBody.get("f.req");
      writeBack = (v) => { rawBody.set("f.req", v); return rawBody; };
    } else if (rawBody instanceof FormData) {
      bodyKind = "formdata";
      const v = rawBody.get("f.req");
      if (typeof v === "string") fReq = v;
      writeBack = (vv) => { rawBody.set("f.req", vv); return rawBody; };
    } else if (rawBody instanceof Blob) {
      bodyKind = "blob";
      try {
        const text = await rawBody.text();
        let params;
        try { params = new URLSearchParams(text); } catch { return rawBody; }
        fReq = params.get("f.req");
        writeBack = (v) => { params.set("f.req", v); return params.toString(); };
      } catch { return rawBody; }
    }

    LOG.debug("main", "gemini-body-kind", { reqId, bodyKind, hasFReq: typeof fReq === "string" });

    if (typeof fReq !== "string" || !writeBack) return rawBody;

    let outer;
    try { outer = JSON.parse(fReq); } catch { return rawBody; }
    if (!Array.isArray(outer) || outer.length < 2 || typeof outer[1] !== "string") {
      LOG.debug("main", "gemini-outer-shape-unexpected", { reqId });
      return rawBody;
    }
    let inner;
    try { inner = JSON.parse(outer[1]); } catch { return rawBody; }
    if (!Array.isArray(inner) || !Array.isArray(inner[0]) || typeof inner[0][0] !== "string") {
      LOG.debug("main", "gemini-inner-shape-unexpected", { reqId });
      return rawBody;
    }
    const userMessage = inner[0][0];
    if (!userMessage) return rawBody;
    if (core.helpers.containsKnownSurrogate(userMessage)) {
      LOG.debug("main", "gemini-skip-has-surrogate", { reqId, len: userMessage.length });
      return rawBody;
    }
    let out;
    try {
      out = await core.helpers.callAnonymize(`${reqId}-gem`, userMessage);
    } catch (err) {
      if (typeof err === "string" && err.startsWith("no_change:")) return rawBody;
      LOG.warn("main", "gemini-anonymize-error", { reqId, error: String(err) });
      return rawBody;
    }
    if (typeof out !== "string" || out === userMessage) return rawBody;
    LOG.debug("main", "fetch-field-anonymized", { reqId, pathTail: "f.req[0][0]", lenBefore: userMessage.length, lenAfter: out.length, changed: true, bodyKind });
    inner[0][0] = out;
    outer[1] = JSON.stringify(inner);
    return writeBack(JSON.stringify(outer));
  }

  api.registerAdapter({
    name: "gemini",
    hosts: [/^gemini\.google\.com$/i],
    chatPatterns: [
      /\/_\/BardChatUi\/data\/.*(?:GenerateContent|StreamGenerate)/i,
      /\/_\/BardChatUi\/data\//,
      /\/v1beta\/models\/.*:streamGenerateContent/,
    ],
    wrapsResponse: false,
    anonymizeBody: anonymizeGeminiBody,
  });
})();
