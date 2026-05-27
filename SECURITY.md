# Security policy

id4pii's entire premise is privacy. Vulnerabilities that would weaken that premise — for example, anything that could cause real PII to be transmitted off the user's machine, or that lets a non-authorized origin reach the local bridge — are taken seriously and patched fast.

## Reporting a vulnerability

**Please do not open a public GitHub issue for security problems.**

Use GitHub's private vulnerability reporting flow instead:

> <https://github.com/TBLgGamin/id4pii/security/advisories/new>

That channel notifies the maintainers privately. A public advisory is published after a fix ships.

When you report, include:

- A description of the vulnerability and what it lets an attacker do.
- Steps to reproduce. A short proof-of-concept beats a long write-up.
- Affected versions, if you know them.
- Suggested remediation (optional).
- Whether you'd like to be credited in the published advisory.

Expect an initial reply within a few days. We do not run a bug bounty.

## What we consider in scope

- The id4pii guard binary (CLI and GUI builds).
- The MV3 Chrome extension shipped from this repository.
- The local WebSocket bridge between the two.
- The Inno Setup installer's behavior during install and uninstall.
- The local vault file format and DPAPI protection.

Concrete examples of things we want to hear about:

- A non-extension origin (web page, other extension, native app) reaches the bridge.
- The model directory, vault file, or log file leak data outside `%LOCALAPPDATA%\id4pii\`.
- The extension transmits message content, vault entries, or PII to any remote host.
- The installer writes to or reads from unexpected locations.
- DPAPI usage flaws that let another local user decrypt the vault.

## What's out of scope

- Bugs in the chat sites (ChatGPT, Claude, Gemini) themselves.
- Bugs in upstream dependencies — please report those to the dependency maintainer. We'll mirror an advisory if our usage propagates the risk.
- Theoretical attacks that require an attacker who already has SYSTEM or the user's own credentials. If they're already at that level, every desktop tool is game.
- The fact that an unsigned installer triggers SmartScreen. That's a known limitation noted in [todo.txt](todo.txt) — signing is on the roadmap.

## Supported versions

The latest release on `main` is the only supported version. The project is small enough that there is no parallel maintenance branch.

| Version | Supported |
|---|---|
| latest tag (`vX.Y.Z`) | ✅ |
| anything older | patch path is "upgrade to latest" |

## A note on the threat model

id4pii is a privacy *layer*, not a privacy *enforcer*. It detects and replaces PII in text headed to LLM chat services on a best-effort basis. The PII model (`openai/privacy-filter`) has false negatives. Users should treat id4pii as a useful safety net, not a guarantee that no PII ever reaches the LLM. Vulnerability reports that hinge on "the model missed a phone number formatted weirdly" are model-quality issues and belong in the regular issue tracker, not under this policy.
