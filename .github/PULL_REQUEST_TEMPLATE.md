<!--
Thanks for the PR. A few things that make review fast:

1. Keep the PR focused. One bug fix, one feature, or one refactor — not all three.
2. Make sure `cargo fmt --all`, `cargo clippy --all-targets`, and `cargo test --workspace` all pass locally.
3. If you touched the extension, reload it unpacked and verify the change end-to-end on at least one of the supported sites.
4. Privacy invariant: no message text, response body, or vault entry is ever logged at any level — only lengths, counts, kinds, durations, request IDs. Please don't add log statements that break this.
5. No code comments unless they explain a non-obvious *why*. See CONTRIBUTING.md.
-->

## Summary

<!-- One or two sentences describing what this PR does and why. -->

## What changed

<!-- Bullet list of the substantive changes. Skip whitespace / rename noise. -->

-
-

## How to test

<!-- The minimum sequence of commands or actions someone needs to reproduce the verification you did. -->

```
```

## Checklist

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets`
- [ ] `cargo test --workspace`
- [ ] If extension code changed, reloaded unpacked and verified on a supported site
- [ ] If shipped surface changed (installer / docs / public API), updated the relevant docs (README, CLAUDE.md, CONTRIBUTING.md, installer/README.md)
- [ ] No new TODO / FIXME left in the diff
