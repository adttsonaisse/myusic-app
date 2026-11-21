# BitChord → Rust (Linux Desktop + GPUI) Migration Map

Living index for multi-session continuity. Points to `DECISIONS.md`, `GLOSSARY.md`, `research/*.md`, and crates without duplicating them.

- **Goal**: Standalone native Linux Desktop music client in Rust + GPUI (`bitchord-desktop`), backed by modular pure-Rust crates (`bitchord-*`), maintaining Listen Together protocol compatibility with the existing Go `backend/`.
- **Reference Source**: `reference/BitChord/` (read-only upstream v1.9.1)
- **ADRs**: [DECISIONS.md](DECISIONS.md) (`ADR-001` .. `ADR-009`)
- **Glossary**: [GLOSSARY.md](GLOSSARY.md)
- **Research Topics** (`rust/research/`):
  - [gpui-linux.md](research/gpui-linux.md) — `done` (`gpui-ce 0.3.x` selected per `ADR-009`)
  - [youtube-extraction.md](research/youtube-extraction.md) — `done` (`reqwest 0.13.5`, `rquickjs 0.14.0`, `yt-dlp` bridge flags, 1 MiB chunked range probing)
  - [linux-audio-engine.md](research/linux-audio-engine.md) — `done` (`symphonia 0.6.1` + `symphonia-adapter-libopus 0.3.0`, `pipewire 0.10.1`, `rtrb 0.4.1`, `mpris-server 0.11.0`, `oo7 0.7.3`)

---

## v1 Core Music Player — Wave Plan

### Wave 0: Pure Domain Foundations (`:shared` Wave 0 leaves)
*No UI, no audio hardware, no network I/O dependencies. Fast `cargo test` loop (3.65 s incremental, 0 warnings, `unsafe_code = "forbid"`).*

| Unit ID | Upstream Kotlin Source | Target Crate | Status | Notes / Verification |
| --- | --- | --- | --- | --- |
| `W0-1` | `shared/.../data/model/*` + `shared/.../data/settings/*` | `crates/bitchord-model` | `done` | 24/24 tests passing (`ModelsTest`, `PlayingRowMatcherTest`, `PlaylistOrderTest`, `ArtistCreditsTest`, `ArtistNameIndexTest`, `ArtistNameMatchTest`, `JoinerLanguageTest`, `VersionTimeMap`, `SearchHistory`, `TopPicks`, `SpotifyMatch`, plus `Canvas` added in `W1-3`). |
| `W0-2` | `shared/.../data/listentogether/*` | `crates/bitchord-jam` | `done` | 15/15 tests passing (`JamInviteTest`, `PartyQueueReadingTest`, `ServerClockTest`, Go backend `camelCase` JSON wire contract). |
| `W0-3` | `shared/.../playback/{EqualizerCurve,EqualizerPreset,PeakLimiter}.kt` + `app/.../EqualizerProcessor.kt` | `crates/bitchord-dsp-core` | `done` | 5 acoustic/DSP suites passing (all 14 `EqualizerProcessorTest` assertions + `TransitionFilter` SVF added in `W1-1`). Zero external deps. |
| `W0-4` | `shared/.../data/sources/{SourceTrackKeys,StreamFormat,TrackMatcher}.kt` | `crates/bitchord-matcher` | `done` | 7 comprehensive suites passing (replicates `SourcesTest`, `TrackIdentityMismatchTest`, `trackMatcher.test.ts` — NFKC, Portuguese `part.`, CJK translations, Hepburn kana→romaji, album version markers, explicit flag, video switch). |

### Wave 1: Queue Engine, Lyrics Engine (17 Sources) & Canvas/Presence
*201/201 workspace tests passing, 0 clippy warnings (`-D warnings`), `cargo fmt --check` clean, `#![forbid(unsafe_code)]`.*

| Unit ID | Upstream Kotlin Source | Target Crate | Status | Notes / Verification |
| --- | --- | --- | --- | --- |
| `W1-1` | `shared/.../playback/{ContextRegistry,QueueBuilder,QueueSections,QueueTimeline,QueueWindow,TransitionFilter}.kt` | `crates/bitchord-queue` + `crates/bitchord-dsp-core` | `done` | 13/13 queue tests + `TransitionFilter` 4th-order Butterworth SVF DSP test passing (`ContextRegistryTest`, `QueueWindowTest`, `QueueSectionsTest`, `QueueTimeline`, `QueueBuilder`). |
| `W1-2` | `shared/.../data/lyrics/*` (29 files: TTML, EnhancedLRC, SyllableRuns, BackgroundVocals, LyricAlignments, LrcWriter, Translation/Romanization, 17 `LyricsSource` providers + `LyricsRepository`) | `crates/bitchord-lyrics` | `done` | 133/133 tests passing (`WordSyncTest`, `LyricAnimationWindowTest`, `LrcLibTest`, `LrcRedTest`, `LrcWriterTest`, `GeniusTest`, `KuGouTest`, `MusixmatchTest`, `BackgroundVocalTest`, `ProviderLyricsTest`, `NewLyricsSourceTest`, `LyricsLookupKeyTest`, `LyricsRepository` racing/cancellation/dedup, `LyricsTranslator` + Input Tools retry + gzip cache). Clean `YouTubeLyricsBackend` trait ready for Wave 2 `bitchord-innertube`. |
| `W1-3` | `shared/.../data/canvas/*` + `shared/.../data/presence/*` | `crates/bitchord-model` + `crates/bitchord-integrations` | `done` | `CanvasArtwork` + `SpotifyCanvasQuery` (3 tests in `bitchord-model`) + `Presence` live-install heartbeat with 2s close debounce, reopen cancellation, `close_blocking`, and no-`Origin` header assertion (4/4 tests in `bitchord-integrations` replicating `PresenceTest`). |

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
