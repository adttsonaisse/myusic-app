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

### Wave 2: InnerTube, Stream Extraction, Integrations, Storage & Headless CLI Front-Door
*284/284 workspace tests passing across 10 crates, 0 clippy warnings (`-D warnings`), `cargo fmt --check` clean, `#![forbid(unsafe_code)]`, Rust MSRV 1.95.*

| Unit ID | Upstream Kotlin Source | Target Crate | Status | Notes / Verification |
| --- | --- | --- | --- | --- |
| `W2-1` | `shared/.../data/innertube/*` + `YtMusicRepository.kt` + `SongInfoRepository.kt` | `crates/bitchord-innertube` | `done` | 30/30 tests passing (`PodcastTest`, `UploadedTracksTest`, `VersionPairTest`, `SongInfoParserTest`, `SAPISIDHASH` cookie priority, `LocaleTags`, `CipherSandbox` on `rquickjs 0.14.0` QuickJS-NG with UTF-16 & memory/timeout bounds, `StreamResolver` native-first + feature-gated `ytdlp-bridge` per `ADR-004` + `BgUtilHttpProvider` + 1 MiB/16 KiB probe + single-flight coalescing; implements `bitchord_lyrics::YouTubeLyricsBackend`). |
| `W2-2` | `shared/.../data/spotify/*` + `app/.../data/scrobbling/*` + `app/.../data/listentogether/ListenTogether.kt` + `shared/.../data/NerdStats.kt` + Discord RPC | `crates/bitchord-integrations` | `done` | 30/30 tests passing (`PresenceTest` 4, `DiscordAudioQualityTest` 5 + mock Unix domain socket IPC test, `NerdStats` telemetry, `LastFmClient` MD5 `api_sig` + `ListenBrainzClient` + `ScrobbleTracker`, `SpotifyLibraryTest` 3 + embed HTML + Liked Songs + widest cover, `ListenTogetherIdleHealthTest` 5 + URL normalization/redaction + monotonic `seq`/`syncQueue` state machine + live loopback WebSocket Bearer/no-Origin integration test). |
| `W2-3` | `app/.../download/{DownloadStore,Downloader,Downloads,FlacTagger,Mp4Tagger,WebmTagger,MediaTagger}.kt` + `EmbeddedLyrics.kt` + `AppSettings.kt` + `SearchHistory.kt` + Linux XDG/SQLite/Secret Service | `crates/bitchord-storage` | `done` | 24/24 tests passing (`AppPaths` XDG dirs, atomic JSON `SettingsStore`, `SecretStore` + `oo7 0.7.3` pure-Rust `native_crypto` Secret Service + `InMemorySecretStore`, `LibraryDb` SQLite WAL via `rusqlite 0.43` linking system `libsqlite3 3.54.1`, `DownloadStoreTest` + magic-byte `sniff` + atomic `.part` commit/abort, `MediaTaggerTest` FLAC/MP4/WebM byte-level taggers + `EmbeddedLyricsTest` word-synced A2 & `.m3u8` sidecar + `lofty 0.25.5` local tag reader). |
| `W2-4` | Headless CLI & deterministic replay harness | `crates/bitchord-cli` | `done` | 3/3 CLI tests + binary smoke verified (`search`, `browse`, `artist`, `song-info`, `lyrics`, `resolve`, `jam`, `spotify`, `storage`, `queue-demo`, all with `--json` & `--data-dir` isolation). |

### Wave 3: Linux Native Audio Engine & Desktop OS Services
*314/314 workspace tests passing across 11 crates, 0 clippy warnings (`-D warnings`), `cargo fmt --check` clean, `#![forbid(unsafe_code)]`, selective `opt-level = 2` for decoders/resampler/DSP.*

| Unit ID | Upstream Kotlin Source | Target Crate | Status | Notes / Verification |
| --- | --- | --- | --- | --- |
| `W3-1` | Replaces `app/.../playback/` (`AudioEngine`, `DspChain`, `LoudnessProcessor`, `SpatialAudioProcessor`, `CrossfadeController`, `SkipSilence`, `ChunkedDataSource`, `PartySync`) | `crates/bitchord-audio` + `crates/bitchord-cli` | `done` | 29/29 audio tests + 4/4 CLI tests passing (`LoudnessProcessorTest` 6/6, `FloatDspChainTest` 4/4, `CrossfadeState` equal-power `sin²+cos²=1` + `1/sqrt(out+in)` blend trim, `SkipSilenceProcessor` 1.0s threshold + skipped-frame tracking, pure-Rust WSOLA `SonicStretcher` 0.5x–2.0x pitch-preserving, `AudioResampler` on `rubato 5.1`, `AudioDecoder` on `symphonia 0.6.1` + `symphonia-adapter-libopus 0.3.0` linking host `libopus 1.6.1` with authoritative bit depth, 1 MiB `ChunkedRangeSource` with 206 verification/32 MiB cache/403 refusal callback, safe zero-alloc RT `PipeWireSink` on `pipewire 0.10.1` + deterministic `NullAudioSink` over `rtrb 0.4.1`, dual-deck gapless/crossfade `AudioEngine`, `PartySyncCoordinator`, and `bitchord-cli play` live PipeWire + `--no-output` headless smoke verified). |
| `W3-2` | Replaces Android `MediaSession`, notifications, and `SleepTimer.kt` | `crates/bitchord-audio` (`mpris`, `sleep_timer`) | `done` | Linux MPRIS2 D-Bus server (`mpris-server 0.11.0` on `zbus 6.5.0`, `org.mpris.MediaPlayer2.bitchord` + `.instance<pid>` fallback, hex-encoded `ObjectPath` track IDs, local `file://` art URIs, silent `Position` + explicit `Seeked` signal, wired to `bitchord-cli play --mpris`) + monotonic `SleepTimer` (`Instant` deadline + `after_track`, all 3 `SleepTimerTest` assertions + lifecycle verified). |

### Wave 4: GPUI Linux Desktop Application (`gpui-ce 0.3.x`, `ADR-009`)
*318/318 workspace + desktop tests passing across 12 crates (`314` core + `4` desktop), 0 clippy warnings (`-D warnings`), `cargo fmt --check` clean, `#![forbid(unsafe_code)]`, live Wayland/Vulkan/PipeWire/MPRIS2/Tray startup smoke verified (`RUNTIME_SMOKE_PASS`). Isolated out of workspace `default-members` per `ADR-007`.*

| Unit ID | Upstream Reference | Target Crate | Status | Notes / Verification |
| --- | --- | --- | --- | --- |
| `W4-1` | App shell, navigation, dynamic artwork palette & mesh gradient, frosted glass theme | `crates/bitchord-desktop` (`theme`, `artwork`, `actions`, `views/{sidebar,titlebar,player_bar}`) | `done` | `gpui-ce 0.3.0` (`wgpu 29.0.4`) + `gpui_ce_platform 0.3.0` (`QuitMode::Explicit`) + `gpui_ce_tokio 0.3.0` (2 workers). Authentic BitChord Dark OLED & Light Studio themes (`Theme.kt`, `#E01E3A` WCAG AA >= 4.5:1 verified by automated contrast test), 64×64 BGRA dynamic artwork mesh backdrop, bounded 64-entry LRU `ArtworkCache` with VRAM eviction, Wayland CSD / X11 adaptive titlebar, collapsible grouped nav rail, floating bottom Now Playing bar (`.backdrop_blur(px(20.0))`). |
| `W4-2` | Home, Search, Album/Artist/Playlist screens, Library & Downloads views | `crates/bitchord-desktop` (`backend`, `text_input`, `views/{home,search,detail,library}`) | `done` | App-level `AppBackend` global coordinating `AudioEngine`, `QueueTimeline`, `YtMusicRepository`, `StreamResolver`, `LyricsRepository`, `LibraryDb`, `SettingsStore`, `KeyringSecretStore`, `DiscordIpcClient`, `SpotifyImporter`, `ListenTogetherClient`, and `MprisService` via `async_channel`. Virtualized `uniform_list` views for Search, Detail, and Library, local Linux folder scanner (`read_local_audio_tags`), SQLite search history & downloads, keyboard/IME-aware `TextInput` (`.key_context("TextInput")`), explicit Loading/Empty/Error states. |
| `W4-3` | Now Playing screen, Apple-like word-synced lyrics, Queue drawer, Listen Together party UI, Settings, Stats/Replay, Nerd Stats, Auth login flow | `crates/bitchord-desktop` (`views/{now_playing,queue_drawer,party,settings}`, `main.rs`) | `done` | Full-bleed Now Playing stage over dynamic artwork mesh, 60fps word-synced lyrics (`LyricWord::chars_sung_at` + variable-height `list()` + `LyricsTranslator` + click-to-seek, zero idle GPU frames), collapsible measured `NerdStats` telemetry, slide-over `.backdrop_blur` Queue drawer with `QueueTier` badges, Listen Together REST+WS room UI, 7-band SVF Equalizer (10 presets + ±1 dB steppers), Crossfade 0–12 s, Sleep Timer, Secret Service YouTube cookie auth, Spotify playlist import, Linux keyboard shortcuts, and `gpui-tray 0.3.0` (`gpui-ce` feature) close-to-tray lifecycle. |

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
