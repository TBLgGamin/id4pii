---
name: add-site
description: Wire up a new LLM chat site (e.g. mistral.ai, perplexity, deepseek) into the id4pii Chrome extension. Creates a new adapter under extension/src/adapters/ and edits manifest.json so the extension intercepts that site's chat requests. Trigger on "add a new site", "wire up <site>", "/add-site <name>", or whenever the user wants the extension to cover another chat product.
---

# add-site

Adds a new chat site to the id4pii browser extension. The extension is built around one adapter per site under `extension/src/adapters/`. Shared logic (fetch/XHR patching, response restoring, vault IPC, DOM mutation observer, JSON walker) lives in `extension/src/main/core.js` — your adapter only owns "what's my host, which URLs are chat requests, how do I dig the user prompt out of the body."

## Inputs

The user invokes `/add-site <name>` or describes the site. From their message, extract:

- **Site key**: a lowercase identifier for the adapter file (`mistral`, `perplexity`, `deepseek`). Used as the adapter `name` and the filename.
- **Hostnames**: the public hosts the extension should activate on (`chat.mistral.ai`, `www.perplexity.ai`). Ask if not provided.

If either is missing, ask via `AskUserQuestion` before doing any work.

## Steps

Use `TaskCreate`/`TaskUpdate` to track these.

### 1. Discover the chat-completion URL(s)

Ask the user to do this once, since it requires having an account on the site:

> Open the site, open DevTools → Network, send a chat message, and copy the **request URL** of the POST that streams the assistant reply. Paste it back. Repeat for any other endpoints (regenerate, retry, branch from message — they often have their own URLs).

Build a regex per endpoint matching the path. Examples already in the repo:

- ChatGPT: `/backend-api\/(?:[^/]+\/)?conversation/`
- Claude: `/api\/organizations\/[^/]+\/chat_conversations\/[^/]+\/completion/`
- Gemini: `/_\/BardChatUi\/data\/.*(?:GenerateContent|StreamGenerate)/i`

Prefer **path patterns** over full URLs so subdomain or region changes don't break the regex.

### 2. Identify the request body shape

Look at the same DevTools request's payload. Two common shapes:

**Generic JSON with user prose in nested fields** (ChatGPT, Claude, most OpenAI-style APIs):
```json
{"messages": [{"role": "user", "content": "..."}], ...}
```
For this shape the adapter just delegates to `core.helpers.anonymizeJsonBody` — the shared walker finds prose-looking strings under keys like `content`, `text`, `prompt`, `message`, or under parents like `messages`, `contents`, `parts`.

**Custom encoding** (Gemini's `f.req` form-encoded JSON-in-JSON): the adapter writes its own extractor. See `extension/src/adapters/gemini.js` as the template.

If unsure, default to the generic JSON path first and only switch to custom if testing shows the walker can't find the user message.

### 3. Create the adapter file

For a JSON-body site (vast majority), copy the chatgpt adapter structure verbatim and swap `name`, `hosts`, `chatPatterns`:

```js
(() => {
  const api = window.__id4pii_main;
  if (!api) return;

  api.registerAdapter({
    name: "<site-key>",
    hosts: [/^<host-regex>$/i],
    chatPatterns: [
      /<chat-url-regex>/,
    ],
    wrapsResponse: true,
    async anonymizeBody(core, reqId, rawBody) {
      const text = await core.helpers.bodyAsString(rawBody);
      if (text == null) return rawBody;
      return await core.helpers.anonymizeJsonBody(reqId, text);
    },
  });
})();
```

Write to `extension/src/adapters/<site-key>.js`. `hosts` regexes are tested against `location.hostname`; `chatPatterns` are tested against full request URLs.

For a custom-encoding site, fork `extension/src/adapters/gemini.js` instead and replace the body extractor. Set `wrapsResponse: false` if the site does **not** stream JSON/SSE/plain text responses that would benefit from the in-line restore transformer (rare — most do).

### 4. Edit `extension/manifest.json`

Two arrays + one resource map need the new host pattern, plus the MAIN-world script array needs the new adapter file. **All three must stay in sync** or the extension will silently no-op on the new site.

1. Both `content_scripts[].matches` arrays: append `"*://<host>/*"` (and `"*://*.<host>/*"` if the site uses subdomains the user sees, like `claude.ai` vs `*.claude.ai`).
2. The MAIN-world `content_scripts[1].js` array: insert `"src/adapters/<site-key>.js"` **before** `"src/main/boot.js"` (boot calls `start()`, which freezes the adapter set).
3. `web_accessible_resources[0].matches`: append the same host patterns. Required so the in-page world can load the lock animation frames on that site.

Do **not** add a `host_permissions` entry. Static content scripts inject without one, and skipping it keeps the extension out of Chrome Web Store's "in-depth review" queue.

### 5. Add the site to the onboarding provider catalog

After the engine connects, the onboarding "You're set" page (`extension/onboarding/onboarding.html`) shows a grid of clickable provider cards. Every supported site must appear there or the user won't have a launcher for it.

**Steps:**

1. **Grab a logo SVG.** Best source is simple-icons (CC0-licensed, brand-correct, monochrome with `currentColor`):
   ```
   curl -sL "https://cdn.jsdelivr.net/npm/simple-icons@v13/icons/<brand-slug>.svg" -o assets/providers/<brand-slug>.svg
   ```
   The slug usually matches the company, not the product (`anthropic` for Claude, `openai` for ChatGPT, `googlegemini` for Gemini). Find the right slug at <https://simpleicons.org/>. Save it under the repo-root `assets/providers/` — the `sync-extension-assets.ps1` script copies the whole directory into `extension/assets/providers/`.

2. **Add a card to the grid** in `extension/onboarding/onboarding.html`. Copy an existing `<a class="provider-card">` block and update:
   - `href` → the user-facing site URL (`https://mistral.ai`, etc.)
   - `<img src="../assets/providers/<brand-slug>.svg" alt="" />` → the SVG you just saved
   - `<span class="provider-name">` → product name as users know it (`Mistral`, `Perplexity`)
   - `<span class="provider-host">` → the bare hostname for visual hint (`chat.mistral.ai`)

3. **Verify the logo renders monochrome.** The CSS applies `filter: invert(98%)` in dark mode so single-path SVGs with black fill come out white on the card. If the SVG has multi-color paths or its own `fill`/`stroke` colors, edit the SVG to use a single `fill="currentColor"` so theming works.

4. **Re-run** `.\scripts\sync-extension-assets.ps1` to copy the new SVG into the extension's bundled assets, then reload the unpacked extension.

The provider cards are not how interception is wired — they're just launcher buttons. The actual interception lives in the adapter you wrote in step 3. The two must stay in sync: if a card exists with no adapter, clicking it opens the site but PII passes through unredacted.

### 6. Update `extension/manifest.json` description if needed

The Chrome Web Store description (currently `"Local PII anonymization for ChatGPT, Claude, and Gemini..."`) lists supported sites by name. If the new site is user-visible (not an internal/beta thing), append it. Keep the description **under 132 characters** — Chrome Web Store rejects longer ones (this caused a real rejection in `de90c18`).

### 7. Tell the user how to test

End the skill with these exact verification steps so they know what's expected:

```
Sync assets and reload the unpacked extension:

  .\scripts\sync-extension-assets.ps1

Then in Chrome: chrome://extensions → id4pii guard → reload icon.

Open the site, open DevTools console, type:
  id4pii.debug(true)

Send a message containing PII (e.g. "I'm Sarah Connor, sarah@skynet.com").
Look for these log lines in console:
  [id4pii][main] boot host=<site> adapters=["<site-key>", ...]
  [id4pii][main] fetch-intercept ... adapter=<site-key>
  [id4pii][main] fetch-body-out ... changed=true

If `adapter=null` shows up instead, the chatPatterns regex doesn't match the
real request URL — re-check the path in DevTools Network.
If changed=false, the body shape doesn't fit the generic JSON walker —
switch to a custom extractor based on gemini.js.
```

## Notes

- Don't commit. Stop after the files are written and let the user verify in their browser first.
- Don't update `CLAUDE.md` or `CONTRIBUTING.md` — those describe the mechanism, not the supported-sites list.
- If `extension/assets/` doesn't exist yet, `sync-extension-assets.ps1` builds it from the repo-root `assets/`. The user already runs this; only flag it if it's missing.
