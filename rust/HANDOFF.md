# BitChord / Myusic Rust Port — Session Handoff

## Current Status — v1 Core Music Player + v2 Audio Advanced & Network Storage COMPLETE!
- **Completed**: Foundation setup + all 3 primary-source research reports + **v1 Waves 0–4** + **QA Rounds 1–2** + **v2 Waves 5A–5D**:
  - **Wave 5A (`55d5139`) — Network Storage**: Pure-Rust `bitchord-netstorage` crate with WebDAV (`PROPFIND` BFS, `MKCOL`, `HEAD`, `PUT`, Basic Auth, folder cover-art pairing) & SMB2/3 (`smb 0.12`, `SmbRangeReader` 128 KiB LRU seekable streaming), Secret Service credential storage, CLI `netstorage` + remote `play`, and Desktop Settings/Library integration.
  - **Wave 5B (`1f834c4`) — Bit-Perfect Exclusive Audio**: Safe `alsa 0.12` hardware output (`AlsaExclusiveSink` on `hw:X,Y`, `enumerate_alsa_devices`, USB DAC detection, `S32_LE`/`S24_3LE`/`FLOAT_LE`/`S16_LE` bit-exact converters), PipeWire dynamic `node.force-rate` native clock switching, honest `bit_perfect_active` audit, CLI `devices` + `play --output-mode`, and Desktop Settings/Now Playing pipeline telemetry.
  - **Wave 5C (`dbdab4e`) — DSD / DST Playback**: Pure-Rust `#![forbid(unsafe_code)]` DSF & DSDIFF/DFF parser (`DSD64`..`DSD1024`), nibble-LUT Blackman-windowed sinc `DsdBlockDecimator` (`DSD -> 176.4 kHz f32`), pure-Rust ISO/IEC 14496-3 Subpart 10 `DstDecoder` + CRC-32 (`0x8000_0011`), `DopPacker` (DoP v1.1 `0x05`/`0xFA` framing), transparent sniffing in `AudioDecoder`, local/WebDAV/SMB scanning, and `MHz · 1-bit` Hi-Res telemetry.
  - **Wave 5D — Automix ONNX + LoopMix**: Pure-Rust `bitchord-automix` crate (`StreamingAnalyzer` + DSP tempo/PLL beat/downbeat/phrase/Krumhansl-Schmuckler key/vocal analyzer, `TransitionPolicy`, `TransitionPlanner` with 8/16-bar phrases, Camelot harmonic scoring, vocal-clash avoidance, DJ Bass Swap & Filter Sweep, `LoopMix` analytical tempo-ramp clock & pure-Rust biquad bridge renderer, exact Beat-This Mel & UMX-HQ STFT spectrograms, plus optional CPU-throttled `--features automix-onnx` dynamic `ort` backend <=2 threads with arena disabled), SQLite schema v2 `track_analysis` cache, live `AudioEngine` RT filter & beatmatched speed execution, CLI `automix {status, analyze, plan}`, and Desktop Settings/Now Playing controls.
- **Locked Decisions**: `ADR-001`..`ADR-009` in `rust/DECISIONS.md` (`gpui-ce 0.3.x`, PipeWire + ALSA Exclusive audio, Go backend protocol parity, laptop compilation caps, per-wave checkpoints).
- **Zero `unsafe` across all 14 workspace crates** (`#![forbid(unsafe_code)]`, Rust MSRV 1.95).

## How to Run Locally
- Launch Desktop App (`Myusic`, Wayland/X11 + PipeWire/ALSA + MPRIS2 + Tray):
  ```bash
  cd /home/adttsonaisse/Documents/Coding/bitchord-rs
  ./target/debug/bitchord-desktop
  # or rebuild incrementally: cargo run -j 2 -p bitchord-desktop
  # optional ONNX Runtime feature: cargo build -j 2 -p bitchord-desktop --features automix-onnx
  ```
- Launch Headless CLI:
  ```bash
  cargo run -p bitchord-cli -- devices
  cargo run -p bitchord-cli -- automix status
  cargo run -p bitchord-cli -- automix analyze /path/to/track.flac
  cargo run -p bitchord-cli -- automix plan /path/to/a.flac /path/to/b.flac --mode advanced
  cargo run -p bitchord-cli -- play /path/to/track.dsf --output-mode pipewire-force-rate
  cargo run -p bitchord-cli -- netstorage webdav-list --url https://nas.local/dav/music --user me --pass secret
  ```

## Remaining Optional Roadmap
1. **Pluggable JS Modules**: Pooled `rquickjs` sandbox (12 modules x 3 VMs).
2. **Linux Packaging**: Arch Linux `PKGBUILD`, `.desktop` entry, SVG/PNG icons, `bitchord://` URI handler, Flatpak/AppImage.

## Key Files
- Plan & unit states: `rust/MAP.md`
- Architecture decisions: `rust/DECISIONS.md` (`ADR-001`..`ADR-009`)
- Domain glossary: `rust/GLOSSARY.md`
- Research reports: `rust/research/{gpui-linux,youtube-extraction,linux-audio-engine}.md`
- Laptop build config: `.cargo/config.toml` (`jobs = 4`, `clang` + `lld --threads=2`) & `Cargo.toml`
