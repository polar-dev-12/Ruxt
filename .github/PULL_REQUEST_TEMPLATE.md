## What and why

## Checklist

- [ ] `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check` pass
- [ ] New UI strings have Korean entries in `src/core/i18n.rs`
- [ ] `CHANGELOG.md` updated under *Unreleased*
- [ ] If this touches auth, host keys, tunnels, X11, the vault or file names: `docs/THREAT_MODEL.md` reviewed and updated
- [ ] No new `unsafe`, no new network egress, no secrets written outside the vault
