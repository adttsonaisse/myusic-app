# BitChord Rust Port — Session Handoff

## Current Status
- **Completed**: Foundation setup + all 3 primary-source research reports + **Wave 0** (`W0-1`..`W0-4`) + **Wave 1** (`W1-1`..`W1-3`).
- **Locked Decisions**: `ADR-001`..`ADR-009` in `rust/DECISIONS.md` (including `ADR-009`: `gpui-ce 0.3.x` for Wave 4).
- **Wave 1 Gate**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` all **PASS**:
  - `bitchord-model`: 24 passed
  - `bitchord-jam`: 15 passed
  - `bitchord-dsp-core`: 5 passed
  - `bitchord-matcher`: 7 passed
  - `bitchord-queue`: 13 passed
  - `bitchord-lyrics`: 133 passed
  - `bitchord-integrations`: 4 passed
  - **Total**: **201/201 tests passing**, 0 warnings, `#![forbid(unsafe_code)]`, Rust MSRV 1.86.
- **Next Wave**: **Wave 2** (awaiting user checkpoint go-ahead per `ADR-008`):
  - `W2-1`: `crates/bitchord-innertube` (native InnerTube browse/search/next/transcript implementing `bitchord_lyrics::YouTubeLyricsBackend` + `StreamResolver` with `rquickjs 0.14.0` native target and feature-gated `yt-dlp` bridge per `rust/research/youtube-extraction.md` & `ADR-004`).
  - `W2-2`: `crates/bitchord-integrations` expansion (Spotify playlist import, Last.fm & ListenBrainz scrobbling, Discord Rich Presence Linux IPC + Gateway, Listen Together WebSocket client).
  - `W2-3`: `crates/bitchord-storage` (SQLite library/history/stats, XDG config/cache, Linux Secret Service `oo7 0.7.3`, Offline Download Manager + `lofty` tagging + embedded lyrics reader).
  - `W2-4`: `crates/bitchord-cli` (headless CLI front-door for search, lyrics, stream resolution, Listen Together, and behavioral replay gating).

## Key Files
- Plan & unit states: `rust/MAP.md`
- Architecture decisions: `rust/DECISIONS.md` (`ADR-001`..`ADR-009`)
- Domain glossary: `rust/GLOSSARY.md`
- Research reports:
  - `rust/research/gpui-linux.md`
  - `rust/research/youtube-extraction.md`
  - `rust/research/linux-audio-engine.md`
- Laptop build config: `.cargo/config.toml` (`jobs = 4`, `clang` + `lld --threads=2`) & `Cargo.toml`
- Skill to load next: `rust-anything` (`whole-app.md`)
