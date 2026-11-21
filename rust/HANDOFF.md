# BitChord Rust Port — Session Handoff

## Current Status
- **Completed**: Foundation setup + all 3 primary-source research reports + **Wave 0** (`W0-1`..`W0-4`).
- **Gate**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` all **PASS** (`47/47` tests, `0` warnings, `unsafe_code = "forbid"`, `3.65s` incremental build+test on i3-1220P / 7.5 GiB RAM).
- **Next Wave**: **Wave 1** — `W1-1` (`crates/bitchord-queue`), `W1-2` (`crates/bitchord-lyrics`, 12+ providers + TTML/EnhancedLRC), `W1-3` (`data/canvas` + `data/presence`).
- **Pending User Decision (for Wave 4)**: Choose GPUI crate line (`gpui 0.2.2` vs `gpui-ce 0.3.0`) based on `rust/research/gpui-linux.md`.

## Key Files
- Plan & unit states: `rust/MAP.md`
- Architecture decisions: `rust/DECISIONS.md` (`ADR-001`..`ADR-008`)
- Domain glossary: `rust/GLOSSARY.md`
- Research reports:
  - `rust/research/gpui-linux.md`
  - `rust/research/youtube-extraction.md`
  - `rust/research/linux-audio-engine.md`
- Laptop build config: `.cargo/config.toml` (`jobs = 4`, `clang` + `lld --threads=2`) & `Cargo.toml`
- Skill to load next: `rust-anything` (`whole-app.md`)
