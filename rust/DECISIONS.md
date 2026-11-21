# BitChord Rust Migration — Architecture Decision Records (ADRs)

Numbered in order and never renumbered. Superseding a decision adds a new ADR referencing the old one.

---

## ADR-001: Target Shape — Pure Rust Core + GPUI Linux Desktop App
Status: accepted (2026-11-21)
Context: BitChord is originally an Android/KMP music client (174.7k LOC Kotlin, 5.5k LOC C++, 2.6k LOC Go backend, 23.7k LOC TS web). The goal is a native Linux desktop client rewritten in Rust.
Decision: Build a Cargo workspace split cleanly into headless domain/audio crates (`crates/bitchord-*`), a lightweight CLI/verification harness (`crates/bitchord-cli`), and a GPU-accelerated Linux desktop UI (`crates/bitchord-desktop`) built with **GPUI** (`gpui` crate).
Consequences:
- Domain logic, lyrics, queue, InnerTube, and audio remain 100% independent of GPUI and compile/test in seconds without Vulkan/Wayland UI deps.
- `bitchord-desktop` is excluded from workspace `default-members` during core-wave work so `cargo test` stays lightweight on modest hardware.

## ADR-002: Platform Scope — Linux Desktop Only, Keep Go Backend
Status: accepted (2026-11-21)
Context: The upstream repo includes Android Phone (`app`), Android TV (`tv`), Wear OS (`wearos`), Android Widgets, Google Cast, a React web player (`web`), and a Go Listen Together server (`backend`).
Decision: Migrate only `:shared` and desktop-relevant features from `:app` to Linux Desktop (x86_64, Wayland + X11, PipeWire + ALSA). Drop `:tv`, `:wearos`, `widget`, `cast`, Android-specific Bluetooth/Keystore code, and `web`. Keep `backend/` in Go and maintain strict camelCase JSON + WebSocket protocol compatibility with `backend/README.md`.
Consequences:
- Eliminates ~17.8k LOC of Android-only targets and Android TEE attestation (`AddonAuth`, which cannot run outside signed official Android APKs).
- Existing Listen Together servers (`api.bitchord.kushagrasingh.in` or self-hosted Go instances) work out of the box with the Linux desktop client.

## ADR-003: Phased Feature Scope — Core Player v1, Advanced Audio & NAS in v2
Status: accepted (2026-11-21)
Context: BitChord carries both core playback features and specialized audiophile/ML subsystems (Automix ONNX beat/vocal models, Signalsmith Stretch loopmix, DSD/DST SACD decoding, bit-perfect USB DAC, SMB2/3, WebDAV).
Decision:
- **v1 (Core Music Player)**: YouTube Music search/browse/playback, Queue & Timeline, Gapless playback + Crossfade, 12+ Lyrics providers (TTML/EnhancedLRC word-synced + translation), Dynamic artwork theming/mesh gradient, Offline downloads with embedded metadata, Local music library scanner, MPRIS2 D-Bus & desktop media keys, Listen Together client, Discord RPC, Last.fm/ListenBrainz scrobbling, Settings/Stats, Linux Secret Service (`keyring`/`oo7`) auth storage, GPUI Desktop UI.
- **v2 (Roadmap)**: Automix ONNX (`ort` + `beat_this`/`vocals_umxhq`) & Signalsmith Stretch LoopMix, DSD/DST playback, Bit-Perfect ALSA `hw:` / USB DAC output, SMB2/3 & WebDAV remote libraries, QuickJS plugin modules (`rquickjs`).
Consequences: Faster path to a rock-solid daily-driver Linux player without blocking v1 on niche DSP/NAS subsystems.

## ADR-004: YouTube Stream Extraction — Native-First with `yt-dlp` Dev Bridge
Status: accepted (2026-11-21)
Context: `rust-anything analyze` flagged `com.metrolist.innertubex`, `org.schabi.newpipe`, `org.mozilla.rhino`, and WebView BotGuard PoToken as `unknown` fit with no 1:1 Rust crate.
Decision: Build a native Rust `bitchord-innertube` crate (InnerTube API + `rquickjs` player JS cipher/n-throttling solver) as the final target. During v1 development, wire a feature-gated `yt-dlp` subprocess resolver behind the same `StreamResolver` trait so audio playback and UI can be developed and verified immediately; remove or make `yt-dlp` fallback optional once native extraction passes full reliability checks.
Consequences: Eliminates early deadlock on YouTube cipher changes while keeping the architecture cleanly native-first.

## ADR-005: Native C/C++ DSP Migration Strategy (v2)
Status: accepted (2026-11-21)
Context: `native/analyzer`, `native/loopmix`, and `app/src/main/cpp/dsd` contain 5.5k LOC of IEEE-754 sensitive C++17/C DSP code (explicitly compiled without `-ffast-math` for analyzer).
Decision: In v2, first wrap the existing C++17/C sources directly via `cc`/`cxx` in `build.rs` for instant Linux DSP parity, then port unit-by-unit to pure Rust (`rustfft`, `rubato`, etc.) verified with a relative float tolerance of `1e-6`.
Consequences: Zero acoustic regression when introducing Automix/DSD on Linux, with a clear pure-Rust lift path.

## ADR-006: Verification & Equivalence Bar (v1)
Status: accepted (2026-11-21)
Context: Local Arch Linux environment has `cargo 1.100.0`, `clang`, `lld`, `yt-dlp`, `ffmpeg`, and `rust-anything 2.1.0`, but does not have JDK/Kotlin installed. BitChord includes 141 Kotlin unit test files plus `web/fixtures/`.
Decision: For v1, verify every ported unit by replicating the upstream Kotlin unit tests (and edge-case semantic traps: UTF-8/UTF-16 offsets, non-ASCII regexes, Unicode NFD normalization, deterministic map order, error paths) in Rust `cargo test` + `clippy -D warnings` + `rustfmt`, plus `bitchord-cli` behavioral tests (`rust-anything app record/replay` once CLI is wired). Installing JDK 21 + `kotlinc` for direct JNI differential testing via `rust-anything record/diff` remains an explicit opt-in fallback if any complex unit needs JVM ground truth.
Consequences: Fast, zero-JVM-bloat development loop with high behavioral fidelity to upstream tests.

## ADR-007: Low-Stress Laptop Compilation Profile
Status: accepted (2026-11-21)
Context: Development hardware is Intel Core i3-1220P (2P+8E, 12 threads) with 7.5 GiB RAM (~3.6 GiB free) and Intel UHD Graphics (Alder Lake, `vulkan-intel` ANV driver installed) on Arch Linux Wayland. Default Cargo builds (12 jobs, GNU `ld`, full DWARF) exhaust RAM and throttle CPU.
Decision:
- `.cargo/config.toml`: `build.jobs = 4`, `linker = "clang"`, `rustflags = ["-C", "link-arg=-fuse-ld=lld"]`.
- `Cargo.toml`: `[profile.dev] debug = "line-tables-only"`, `[profile.dev.package."*"] debug = 0`, `opt-level = 0` (selective `opt-level = 2` only for audio decoders once added in Wave 3), `[profile.release] lto = "thin"`, `codegen-units = 4`, `strip = "symbols"`.
- Isolate `crates/bitchord-desktop` (GPUI) out of `default-members` so core waves never trigger GPUI/Vulkan recompilation.
Consequences: Fast incremental builds, low memory footprint (<2.5 GiB peak), responsive desktop during compilation.

## ADR-008: Workspace Layout & Checkpoints
Status: accepted (2026-11-21)
Context: Active editor workspace is an unrelated portfolio repo (`adtt-portofolio`).
Decision: Develop in `/home/adttsonaisse/Documents/Coding/bitchord-rs` with upstream BitChord kept read-only in `reference/BitChord/` (gitignored). Pause for user review and update `rust/MAP.md` + `rust/HANDOFF.md` at the completion of every Wave.
Consequences: Clean separation from other projects and predictable multi-session continuity.

## ADR-009: GPUI Crate Line — `gpui-ce 0.3.x` (`wgpu`) Over Official `gpui 0.2.2`
Status: accepted (2026-11-21)
Context: Primary-source research (`rust/research/gpui-linux.md`) revealed that official `gpui 0.2.2` (`blade-graphics`) has a `BladeAtlas::remove()` VRAM leak risking exhaustion on the 7.5 GiB shared-VRAM Intel UHD laptop, lacks `QuitMode::Explicit` for close-to-tray, and lacks CSS `backdrop_filter` blur. `gpui-ce 0.3.0` (`wgpu`, MSRV 1.95, compatible with local `rustc 1.100.0`) fixes atlas deallocation, adds native CSS `.backdrop_filter(Filter::Blur(...))`, `custom-gpu` shaders, `QuitMode::Explicit`, `gpui_ce_tokio 0.3.0`, and `gpui-tray 0.3.0` (`features = ["gpui-ce"]`, pure-Rust `zbus`, zero GTK3).
Decision: Target `gpui-ce = "0.3"` + `gpui_ce_tokio = "0.3"` + `gpui-tray = "0.3"` (`gpui-ce` feature) for `crates/bitchord-desktop` in Wave 4. Keep `bitchord-desktop` outside workspace `default-members` and cap Tokio worker threads to 2 (GPUI already spawns 12 background workers on this CPU).
Consequences: Native frosted-glass blur, safe bounded GPU atlas memory on shared-VRAM laptops, clean Wayland close-to-tray, and zero GTK3/WebKitGTK bloat.
