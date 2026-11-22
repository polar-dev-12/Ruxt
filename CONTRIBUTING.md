# Contributing to Ruxt

Thanks for helping. Ruxt is a security tool, so the bar for changes that touch
authentication, host keys, tunnels, X11, the vault or file-name handling is higher than for
UI work: explain the threat you are addressing or the one you are not introducing.

## Before you open a pull request

Run exactly what CI runs:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo deny check          # licences, advisories, duplicate crates (cargo install cargo-deny)
```

Rules the test suite enforces:

* Every UI string goes through `tr("…")` / `trf("…")` and needs a Korean entry in
  `src/core/i18n.rs` (`every_ui_string_has_a_korean_translation` scans the UI sources).
* `#![forbid(unsafe_code)]` stays. If something truly needs FFI, discuss it first.
* New files that hold user data are written with `config::write_private_atomic` (owner-only,
  atomic) and never contain secrets outside the vault.

## Design notes worth reading first

* `docs/THREAT_MODEL.md` — what Ruxt defends against and the known limitations. A change
  that weakens a row there needs a very good reason and a documentation update.
* `context.md` — the running hand-over document with the non-obvious decisions (grid-based
  command tracking, X SECURITY cookie flow, egui layout pitfalls, Cygwin/X rebuild).
* `src/app/kit.rs` — the layout vocabulary (cards, bounded windows, button hierarchy). New
  dialogs should use it instead of raw `egui::Window` / `egui::Modal`.

## Reporting bugs

Use the issue templates. For anything security-relevant, read `SECURITY.md` first and do
not post details publicly.

## Licence

By contributing you agree that your contribution is licensed under MIT OR Apache-2.0, like
the rest of the project.
