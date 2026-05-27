(() => {
  const api = window.__id4pii_main;
  if (!api) return;

  api.registerAdapter({
    name: "claude",
    hosts: [/(^|\.)claude\.ai$/i],
    chatPatterns: [
      /\/api\/organizations\/[^/]+\/chat_conversations\/[^/]+\/completion/,
      /\/api\/organizations\/[^/]+\/chat_conversations\/[^/]+\/retry_completion/,
      /\/api\/append_message/,
      /\/v1\/messages/,
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
