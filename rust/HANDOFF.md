# BitChord Rust Port — Session Handoff

## Current Status
- **Completed**: Foundation setup + all 3 primary-source research reports + **Wave 0** (`W0-1`..`W0-4`) + **Wave 1** (`W1-1`..`W1-3`) + **Wave 2** (`W2-1`..`W2-4`).
- **Locked Decisions**: `ADR-001`..`ADR-009` in `rust/DECISIONS.md` (including `ADR-009`: `gpui-ce 0.3.x` for Wave 4).
- **Wave 2 Gate**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and `bitchord-cli` binary smoke tests all **PASS**:
  - `bitchord-model`: 24 passed
  - `bitchord-jam`: 15 passed
  - `bitchord-dsp-core`: 5 passed
  - `bitchord-matcher`: 7 passed
  - `bitchord-queue`: 13 passed
  - `bitchord-lyrics`: 133 passed
  - `bitchord-innertube`: 30 passed
  - `bitchord-integrations`: 30 passed
  - `bitchord-storage`: 24 passed
  - `bitchord-cli`: 3 passed
  - **Total**: **284/284 tests passing**, 0 warnings, `#![forbid(unsafe_code)]`, Rust MSRV 1.95 (aligned with `oo7 0.7.3` & `gpui-ce 0.3.x`).
- **Next Wave**: **Wave 3** (awaiting user checkpoint go-ahead per `ADR-008`):
  - `W3-1`: `crates/bitchord-audio` (`symphonia 0.6.1` + `symphonia-adapter-libopus 0.3.0` + `pipewire 0.10.1` + `libspa 0.10.1` + `rtrb 0.4.1` + `rubato 5.1.0`; dual-deck gapless preload, 0–12 s equal-power crossfade with blend headroom trim, 1.0 s skip-silence, 0.5x–2.0x pitch-preserving playback speed, SpatialAudio + `bitchord-dsp-core` 10-slot SVF EQ + PeakLimiter/Loudness per-deck DSP chain, 1 MiB `ChunkedRangeSource` HTTP streamer, `NerdStats` measured telemetry; wire `play` subcommand into `bitchord-cli`).
  - `W3-2`: Linux MPRIS2 D-Bus server (`mpris-server 0.11.0` unified on `zbus 6.5.0`) + monotonic `SleepTimer` (`Instant` deadline + `afterTrack`).
  - Remember dev-profile selective `opt-level = 2` for `symphonia-*`, `rubato`, and `bitchord-dsp-core` in `Cargo.toml` per `rust/research/linux-audio-engine.md` §3 rule 8 so debug playback never xruns on the laptop!

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
