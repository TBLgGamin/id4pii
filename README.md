<div align="center">

<img src="assets/icon-128.png" alt="id4pii" width="96" height="96" />

# id4pii

### Talk to ChatGPT, Claude, and Gemini without giving them your real name.

A tiny local privacy layer for AI chat. Type whatever you want — real names, real emails, real addresses. id4pii quietly rewrites them to harmless stand-ins **before** the request leaves your machine, and puts the real values back into the reply so reading stays normal.

The model never sees your real data. Nothing leaves your computer.

[![License: MIT](https://img.shields.io/badge/license-MIT-2EB88A.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows-blue.svg)]()

### [Get id4pii for Windows](https://github.com/TBLgGamin/id4pii/releases/latest) · [Add to Chrome](https://chromewebstore.google.com/)

</div>

---

## See it in action

<div align="center">

<img src="assets/promotional/chatgpt-pii.webp" alt="id4pii on ChatGPT — real PII swapped for fictional stand-ins before submit" width="100%" />

<br/><br/>

<img src="assets/promotional/claude-pii.webp" alt="id4pii on Claude — same anonymize/restore flow" width="100%" />

<br/><br/>

<img src="assets/promotional/gemini-pii.webp" alt="id4pii on Gemini — same anonymize/restore flow" width="100%" />

</div>

---

## Why you might want this

Every prompt you send to a chatbot may carry things you'd rather not hand over. A colleague's name. A customer's email. A home address you pasted while debugging a form. Even when the provider promises not to train on it, that data still leaves your machine and lands on their logs, searchable, leak-able, subpoena-able.

id4pii is the simplest fix: a small local app and a tiny browser extension that work together to make the chatbot see _Sarah Connor_ and *sarah@example.com* instead of you and your real friends.

When the bot replies, you see the real names again. The whole swap is invisible.

---

## How to install

You only need **one of these two**. Whichever you pick, it'll walk you through hooking up the other.

### Installer

1. [**Download `id4pii-setup.exe`**](https://github.com/TBLgGamin/id4pii/releases/latest)
2. Run it. It downloads the AI model, sets itself to start with Windows, and tells Chrome to offer the extension on next launch.
3. Open Chrome → click **Enable** on the prompt → done.

### Chrome extension

1. [**Add id4pii guard from the Chrome Web Store**](https://chromewebstore.google.com/) _(link goes live with the first public release)_
2. A welcome tab opens. If the local app isn't installed yet, it gives you a one-click download link.
3. ~2 minutes of model download, and you're set. The badge turns green.

---

## What you get
**Eight types of PII detected** names, emails, phone numbers, addresses, URLs, dates, account numbers, generic secrets

**Anywhere on Windows, not just browsers** press `Ctrl+Shift+A` in _any_ app's text field (Claude Desktop, Slack, Notepad, you name it) and id4pii rewrites it in place. `Ctrl+Shift+Z` restores it. `Ctrl+Shift+U` undoes.

**Genuinely local** no proxy, no certificate, no cloud account, no telemetry. The two pieces talk to each other on your own loopback address.

**Free and open source** under the MIT license. Fork it, ship it, embed it, your call.

---

## How it works (the short version)

```
┌────────────┐         ┌────────────────┐         ┌────────────┐
│  ChatGPT   │  send   │  id4pii guard  │  local  │ AI privacy │
│   in your  │ ──────► │ (in your tray) │ ──────► │   model    │
│   browser  │ ◄────── │                │ ◄────── │            │
└────────────┘  reply  └────────────────┘         └────────────┘
       ▲                       │
       │  invisible restore    │  remembers: "Sarah" = "Alice"
       └───────────────────────┘
```

1. You hit **Send** on chatgpt.com.
2. The extension intercepts your prompt and hands it to the little app running in your tray.
3. The app spots the personal stuff and swaps each piece for a harmless stand-in. It remembers what it swapped, but only in memory on _your_ computer.
4. The chatbot sees the harmless version. It replies normally.
5. As the reply streams in, your real names quietly slot back into the page. You read your conversation as if nothing happened.

---

## I'm a developer

Cool, start with **[CONTRIBUTING.md](CONTRIBUTING.md)** for the dev setup (Rust toolchain, the `.env` you'll need, building the installer locally). **[CLAUDE.md](CLAUDE.md)** has the architecture, the CLI surface, the HTTP API, debugging knobs, and the full internals.

PRs are welcome, bug reports, new chat sites, packaging improvements, doc fixes, whatever.

## License

[MIT](LICENSE). Use it, fork it, ship it commercially, just don't blame me if it breaks.
