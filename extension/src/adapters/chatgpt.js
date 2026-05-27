(() => {
  const api = window.__id4pii_main;
  if (!api) return;

  api.registerAdapter({
    name: "chatgpt",
    hosts: [/^chatgpt\.com$/i, /(^|\.)openai\.com$/i],
    chatPatterns: [
      /\/backend-api\/(?:[^/]+\/)?conversation/,
      /\/backend-anon\/(?:[^/]+\/)?conversation/,
      /\/v1\/chat\/completions/,
    ],
    wrapsResponse: true,
    async anonymizeBody(core, reqId, rawBody) {
      const text = await core.helpers.bodyAsString(rawBody);
      if (text == null) return rawBody;
      const safe = await core.helpers.anonymizeJsonBody(reqId, text);
      return safe;
    },
  });
})();
