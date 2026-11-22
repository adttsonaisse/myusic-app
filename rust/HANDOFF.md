# BitChord Rust Port — Session Handoff

## Current Status — v1 Core Music Player COMPLETE!
- **Completed**: Foundation setup + all 3 primary-source research reports + **Wave 0** (`W0-1`..`W0-4`) + **Wave 1** (`W1-1`..`W1-3`) + **Wave 2** (`W2-1`..`W2-4`) + **Wave 3** (`W3-1`..`W3-2`) + **Wave 4** (`W4-1`..`W4-3`).
- **Locked Decisions**: `ADR-001`..`ADR-009` in `rust/DECISIONS.md` (`gpui-ce 0.3.x`, PipeWire native audio, Go backend protocol parity, laptop compilation caps, per-wave checkpoints).
- **Final v1 Gate** (`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo clippy -j 2 -p bitchord-desktop --all-targets -- -D warnings`, `cargo test`, `cargo test -j 2 -p bitchord-desktop`, `cargo build -j 2 -p bitchord-desktop`, and 4s live Wayland/Vulkan/PipeWire/MPRIS2/Tray startup smoke test): **ALL PASS**:
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
  - `bitchord-desktop`: 4 passed (+ live Wayland/Vulkan/PipeWire/Tray startup smoke PASS)
  - **Total**: **318/318 tests passing across 12 crates**, 0 clippy warnings, `#![forbid(unsafe_code)]`, Rust MSRV 1.95.

## How to Run Locally
- Launch Desktop App (Wayland/X11 + PipeWire + MPRIS2 + Tray):
  ```bash
  cd /home/adttsonaisse/Documents/Coding/bitchord-rs
  ./target/debug/bitchord-desktop
  # or rebuild incrementally: cargo run -j 2 -p bitchord-desktop
  ```
- Launch Headless CLI:
  ```bash
  cargo run -p bitchord-cli -- search "Kavinsky Nightcall"
  cargo run -p bitchord-cli -- play /path/to/track.flac --mpris --eq-preset bass-boost
  ```

## Next Roadmap (v2 — Deferred per `ADR-003`)
1. **Automix ONNX & LoopMix**: Wrap `native/analyzer` + `native/loopmix` via `cc`/`cxx` + `ort` (`beat_this_final0_q8.onnx`, `vocals_umxhq_int8.onnx`), then lift to pure Rust (`1e-6` tolerance) + port `shared/.../playback/smart/`.
2. **DSD / DST Playback**: Wrap `app/src/main/cpp/dsd/` via FFI then lift to pure Rust.
3. **Bit-Perfect Exclusive Audio**: ALSA `hw:card,dev` exclusive output + PipeWire Pro Audio force-rate + optional `nusb` direct USB DAC.
4. **Network Storage**: SMB2/3 (`pavao`/`smb`) and WebDAV (`reqwest-dav`).
5. **Pluggable JS Modules**: Pooled `rquickjs` sandbox (12 modules × 3 VMs).
6. **Linux Packaging**: Arch Linux `PKGBUILD`, `.desktop` entry, SVG/PNG icons, `bitchord://` URI handler, Flatpak/AppImage.

## Key Files
- Plan & unit states: `rust/MAP.md`
- Architecture decisions: `rust/DECISIONS.md` (`ADR-001`..`ADR-009`)
- Domain glossary: `rust/GLOSSARY.md`
- Research reports: `rust/research/{gpui-linux,youtube-extraction,linux-audio-engine}.md`
- Laptop build config: `.cargo/config.toml` (`jobs = 4`, `clang` + `lld --threads=2`) & `Cargo.toml`
