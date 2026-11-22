# BitChord Rust Port — Session Handoff

## Current Status
- **Completed**: Foundation setup + all 3 primary-source research reports + **Wave 0** (`W0-1`..`W0-4`) + **Wave 1** (`W1-1`..`W1-3`) + **Wave 2** (`W2-1`..`W2-4`) + **Wave 3** (`W3-1`..`W3-2`).
- **Locked Decisions**: `ADR-001`..`ADR-009` in `rust/DECISIONS.md` (`gpui-ce 0.3.x` selected for Wave 4, laptop resource caps in `.cargo/config.toml`, per-wave checkpoints).
- **Wave 3 Gate**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and `bitchord-cli play --no-output --json` binary smoke tests all **PASS**:
  - `bitchord-model`: 24 passed
  - `bitchord-jam`: 15 passed
  - `bitchord-dsp-core`: 5 passed
  - `bitchord-matcher`: 7 passed
  - `bitchord-queue`: 13 passed
  - `bitchord-lyrics`: 133 passed
  - `bitchord-innertube`: 30 passed
  - `bitchord-integrations`: 30 passed
  - `bitchord-storage`: 24 passed
  - `bitchord-audio`: 29 passed
  - `bitchord-cli`: 4 passed
  - **Total**: **314/314 tests passing across 11 crates**, 0 warnings, `#![forbid(unsafe_code)]`, Rust MSRV 1.95.
- **Next Wave**: **Wave 4** (awaiting user checkpoint go-ahead per `ADR-008`):
  - `crates/bitchord-desktop` on **`gpui-ce 0.3.x`** (`ADR-009` + `rust/research/gpui-linux.md`), kept **out of root `Cargo.toml` `default-members`** so headless crates remain fast and low-RAM to compile on the laptop!
  - `W4-1`: App shell, Wayland/X11 `wgpu` window (`QuitMode::Explicit`), `gpui_ce_tokio 0.3.0` Tokio bridge, dynamic artwork palette & animated mesh gradient shader/backdrop blur (`.backdrop_filter(Filter::Blur(...))`), frosted glass theme tokens, sidebar + top bar + persistent bottom mini-player layout.
  - `W4-2`: Home feed, Search, Album/Artist/Playlist detail screens, Library & Downloads views, virtualized track lists, bounded LRU artwork cache.
  - `W4-3`: Full Now Playing view, Apple-like word-synced lyrics renderer (`bitchord-lyrics`), Queue drawer (`bitchord-queue`), Listen Together party UI (`bitchord-integrations` + `PartySyncCoordinator`), Settings/EQ/Crossfade/SleepTimer/NerdStats/Auth dialogs, `gpui-tray 0.3.0` (`gpui-ce` feature) system tray & close-to-tray, MPRIS2 live wiring.

## Key Files
- Plan & unit states: `rust/MAP.md`
- Architecture decisions: `rust/DECISIONS.md` (`ADR-001`..`ADR-009`)
- Domain glossary: `rust/GLOSSARY.md`
- Research reports:
  - `rust/research/gpui-linux.md` (essential before starting Wave 4!)
  - `rust/research/youtube-extraction.md`
  - `rust/research/linux-audio-engine.md`
- Laptop build config: `.cargo/config.toml` (`jobs = 4`, `clang` + `lld --threads=2`) & `Cargo.toml`
- Skill to load next: `rust-anything` (`whole-app.md`)
