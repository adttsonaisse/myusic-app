# BitChord → Rust (Linux Desktop + GPUI) Migration Map

Living index for multi-session continuity. Points to `DECISIONS.md`, `GLOSSARY.md`, `research/*.md`, and crates without duplicating them.

- **Goal**: Standalone native Linux Desktop music client in Rust + GPUI (`bitchord-desktop`), backed by modular pure-Rust crates (`bitchord-*`), maintaining Listen Together protocol compatibility with the existing Go `backend/`.
- **Reference Source**: `reference/BitChord/` (read-only upstream v1.9.1)
- **ADRs**: [DECISIONS.md](DECISIONS.md) (`ADR-001` .. `ADR-008`)
- **Glossary**: [GLOSSARY.md](GLOSSARY.md)
- **Research Topics** (`rust/research/`):
  - [gpui-linux.md](research/gpui-linux.md) — `done` (flags `gpui 0.2.2` vs `gpui-ce 0.3.0` choice before Wave 4)
  - [youtube-extraction.md](research/youtube-extraction.md) — `done` (`reqwest 0.13.5`, `rquickjs 0.14.0`, `yt-dlp` bridge flags, 1 MiB chunked range probing)
  - [linux-audio-engine.md](research/linux-audio-engine.md) — `done` (`symphonia 0.6.1` + `symphonia-adapter-libopus 0.3.0`, `pipewire 0.10.1`, `rtrb 0.4.1`, `mpris-server 0.11.0`, `oo7 0.7.3`)

---

## v1 Core Music Player — Wave Plan

### Wave 0: Pure Domain Foundations (`:shared` Wave 0 leaves)
*No UI, no audio hardware, no network I/O dependencies. Fast `cargo test` loop (3.65 s incremental, 0 warnings, `unsafe_code = "forbid"`).*

| Unit ID | Upstream Kotlin Source | Target Crate | Status | Notes / Verification |
| --- | --- | --- | --- | --- |
| `W0-1` | `shared/.../data/model/*` + `shared/.../data/settings/*` | `crates/bitchord-model` | `done` | 21/21 tests passing (`ModelsTest`, `PlayingRowMatcherTest`, `PlaylistOrderTest`, `ArtistCreditsTest`, `ArtistNameIndexTest`, `ArtistNameMatchTest`, `JoinerLanguageTest`, `VersionTimeMap`, `SearchHistory`, `TopPicks`, `SpotifyMatch`). |
| `W0-2` | `shared/.../data/listentogether/*` | `crates/bitchord-jam` | `done` | 15/15 tests passing (`JamInviteTest`, `PartyQueueReadingTest`, `ServerClockTest`, Go backend `camelCase` JSON wire contract). |
| `W0-3` | `shared/.../playback/{EqualizerCurve,EqualizerPreset,PeakLimiter}.kt` + `app/.../EqualizerProcessor.kt` | `crates/bitchord-dsp-core` | `done` | 4 acoustic/DSP suites passing (replicates all 14 `EqualizerProcessorTest` sine-correlation, shelf, tone-pad, focus, balance, `PeakLimiter` ceiling, bit-exact passthrough, glide-down, and preset checks). Zero external deps. |
| `W0-4` | `shared/.../data/sources/{SourceTrackKeys,StreamFormat,TrackMatcher}.kt` | `crates/bitchord-matcher` | `done` | 7 comprehensive suites passing (replicates `SourcesTest`, `TrackIdentityMismatchTest`, `trackMatcher.test.ts` — NFKC, Portuguese `part.`, CJK translations, Hepburn kana→romaji, album version markers, explicit flag, video switch). |

## Open Questions (Before Wave 4)
- **GPUI Crate Line**: Official Zed `gpui = "0.2.2"` (monolithic `blade-graphics`, known `BladeAtlas::remove()` VRAM leak on shared-VRAM GPUs, no CSS backdrop blur) vs `gpui-ce = "0.3.0"` (`wgpu`, fixed atlas deallocation, native CSS `.backdrop_filter(Filter::Blur(...))`, `QuitMode::Explicit`, `gpui_ce_tokio`, `gpui-tray` support). See [gpui-linux.md](research/gpui-linux.md).

### Wave 1: Queue Engine & Lyrics Engine (12+ Providers)
| Unit ID | Upstream Kotlin Source | Target Crate | Status | Notes / Verification |
| --- | --- | --- | --- | --- |
| `W1-1` | `shared/.../playback/{ContextRegistry,QueueBuilder,QueueSections,QueueTimeline,QueueWindow,TransitionFilter}.kt` | `crates/bitchord-queue` | `open` | Replicate `ContextRegistryTest`, `QueueWindowTest`, `QueueTimeline` tests. |
| `W1-2` | `shared/.../data/lyrics/*` (29 files: TTML, EnhancedLRC, SyllableRuns, BackgroundVocals, LyricAlignments, Translation, 12 providers) | `crates/bitchord-lyrics` | `open` | Replicate `LrcLibTest`, `LrcRedTest`, `LrcWriterTest`, `GeniusTest`, `KuGouTest`, `MusixmatchTest`, `BackgroundVocalTest`, `LyricsLookupKeyTest`. Careful with UTF-8 vs UTF-16 offsets! |
| `W1-3` | `shared/.../data/canvas/*` + `shared/.../data/presence/*` | `crates/bitchord-model` / `crates/bitchord-integrations` | `open` | Canvas artwork normalization + presence helpers. Replicate `PresenceTest`. |

### Wave 2: InnerTube, Stream Extraction, Integrations & Headless CLI Front-Door
*Depends on `rust/research/youtube-extraction.md`.*

| Unit ID | Upstream Kotlin Source | Target Crate | Status | Notes / Verification |
| --- | --- | --- | --- | --- |
| `W2-1` | `shared/.../data/innertube/*` + `YtMusicRepository.kt` + `SongInfoRepository.kt` | `crates/bitchord-innertube` | `open` | Native InnerTube browse/search/next + `StreamResolver` trait (native `rquickjs` cipher target + `yt-dlp` dev bridge per ADR-004). Replicate `PodcastTest`, `UploadedTracksTest`, `VersionPairTest`, `SongInfoParserTest`. |
| `W2-2` | `shared/.../data/spotify/*` + `app/.../data/scrobbling/*` + `shared/.../com/my/kizzy/*` | `crates/bitchord-integrations` | `open` | Spotify import, Last.fm & ListenBrainz scrobbling, Discord Rich Presence (Linux IPC + Gateway), Listen Together WebSocket client. |
| `W2-3` | Local SQLite store, XDG settings, Linux Secret Service (`oo7`/`keyring`), Offline Download Manager (`lofty` metadata) | `crates/bitchord-storage` | `open` | Replaces `SharedPreferences`, `EncryptedPrefs`, Android MediaStore, `DownloadStore`. Replicate `DownloadStoreTest`, `MediaTaggerTest`. |
| `W2-4` | Headless CLI & replay harness | `crates/bitchord-cli` | `open` | Search, inspect lyrics, resolve stream, test Listen Together, and play from terminal. Enables `rust-anything app record/replay` behavioral gating. |

### Wave 3: Linux Native Audio Engine & Desktop OS Services
*Depends on `rust/research/linux-audio-engine.md`.*

| Unit ID | Upstream Kotlin Source | Target Crate | Status | Notes / Verification |
| --- | --- | --- | --- | --- |
| `W3-1` | Replaces `app/.../playback/` (Media3/ExoPlayer) | `crates/bitchord-audio` | `open` | `symphonia` (+ optional FFmpeg for ALAC/HLS/DASH) + `cpal`/`pipewire` output, gapless preload, true crossfade 0–12s, speed 0.5x–2.0x, skip silence, EQ + PeakLimiter integration, NerdStats telemetry. |
| `W3-2` | Replaces Android `MediaSession` & notifications | `crates/bitchord-audio` / `mpris` | `open` | Linux MPRIS2 D-Bus server (`zbus`), media keys, track metadata + cover art exposure, sleep timer. |

### Wave 4: GPUI Linux Desktop Application
*Depends on `rust/research/gpui-linux.md`. Isolated from workspace `default-members`.*

| Unit ID | Upstream Reference | Target Crate | Status | Notes / Verification |
| --- | --- | --- | --- | --- |
| `W4-1` | App shell, navigation, dynamic artwork palette & mesh gradient, frosted glass theme | `crates/bitchord-desktop` | `open` | GPUI Wayland/X11 window on Intel UHD Vulkan, sidebar/top-bar desktop layout, theme tokens. |
| `W4-2` | Home, Search, Album/Artist/Playlist screens, Library & Downloads views | `crates/bitchord-desktop` | `open` | Async tokio bridge into GPUI entities, virtualized track lists, image cache. |
| `W4-3` | Now Playing screen, Apple-like word-synced lyrics, Queue drawer, Listen Together party UI, Settings, Stats/Replay, Nerd Stats, Auth login flow | `crates/bitchord-desktop` | `open` | Smooth 60fps animations, keyboard shortcuts, system tray / close-to-tray. |

---

## v2 Roadmap (Deferred After v1 Core Player Is Stable)
- **Automix ONNX & LoopMix**: Wrap `native/analyzer` + `native/loopmix` (Signalsmith Stretch) via `cc`/`cxx` + `ort` (`beat_this_final0_q8.onnx`, `vocals_umxhq_int8.onnx`), then lift to pure Rust (`1e-6` tolerance) + port `shared/.../playback/smart/`.
- **DSD / DST Playback**: Wrap `app/src/main/cpp/dsd/` via FFI then lift to pure Rust.
- **Bit-Perfect Exclusive Audio**: ALSA `hw:card,dev` exclusive output + PipeWire Pro Audio force-rate + optional `nusb` direct USB DAC.
- **Network Storage**: SMB2/3 (`smb`/`pavao`) and WebDAV (`reqwest-dav`).
- **Pluggable JS Modules**: `rquickjs` pooled sandbox replacing `QuickJsExecutor` (LRU 12 modules × 3 VMs).
- **Linux Packaging**: Arch Linux `PKGBUILD`, `.desktop`, icons, `bitchord://` URI handler, Flatpak/AppImage.

---

## Explicitly Out of Scope (Dropped)
- `:tv` (Android TV, 10.9k LOC)
- `:wearos` & `app/.../wear/` (Wear OS + ADB watch installer, 4.3k LOC)
- `app/.../widget/` (Android Home Screen Widgets, 1.6k LOC)
- `app/.../playback/cast/` (Google Play Services Cast, 0.9k LOC)
- `app/.../data/sources/addon/AddonAuth.kt` (Android Hardware Keystore TEE attestation — impossible outside official signed Android APK)
- `backend/` Go rewrite (kept in Go; protocol compatibility preserved)
- `web/` React web app & browser extension
