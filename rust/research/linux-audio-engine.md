# Linux Native Audio Engine & Desktop OS Services in Rust

> Research report for Wave 3 (`crates/bitchord-audio`, `crates/bitchord-storage`, desktop OS services in `crates/bitchord-desktop`).
> Verified against upstream BitChord v1.9.1 (`reference/BitChord/`), crates.io primary metadata (`cargo info` / crates.io API), and the target host environment on **2026-11-21**.
>
> **Target Host Baseline (measured):** Arch Linux `x86_64` (Intel Core i3-1220P, 7.5 GiB RAM), `rustc 1.100.0` / `cargo 1.100.0`, `libpipewire-0.3` `1.6.11` (`pipewire-pulse` + PipeWire-ALSA active, default graph `48000 Hz` / `clock.allowed-rates = [ 48000 ]`, quantum `1024`), `alsa` `1.2.16.1`, `libopus` `1.6.1`, `sqlite3` `3.54.1`, `dbus-1` `1.16.4`, `FFmpeg n9.0.2` (`libavcodec 63.1.102`, `libavformat 63.1.102`), `gstreamer-1.0` `1.28.9`, desktop `Cutefish` on `Wayland` (`org.kde.StatusNotifierWatcher` owned by `cutefish-compos`, `org.freedesktop.secrets` owned by `gnome-keyring-daemon`).

---

## 1. BitChord Upstream Audio & Playback Requirements

Extracted directly from `reference/BitChord/app/src/main/java/com/music/bitchord/playback/`, `shared/src/commonMain/kotlin/com/music/bitchord/playback/`, `shared/src/jvmSharedMain/kotlin/com/music/bitchord/data/NerdStats.kt`, and `app/src/main/java/com/music/bitchord/download/`.

| Subsystem | Upstream Reference | Required v1 Behavior & Exact Constants |
| --- | --- | --- |
| **Canonical DSP Chain** | `playback/audio/DspChain.kt`, `PrecisionAudioSink.kt`, `PcmBoundary.kt` | Pure interleaved `f32` pipeline with **no intermediate fixed-point clamping**: `SpatialAudioProcessor` → `EqualizerProcessor` → `TransitionFilterProcessor` (v2 Automix) → `LoudnessProcessor` (final limiter). Every stage **self-bypasses without touching samples** when idle/settled so 16/24-bit power-of-two scaling (`1/32768`, `1/8388608`) round-trips bit-exact. Sanitize `NaN`→`0.0`, `±Inf`→`±1.0`. |
| **Graphic EQ & Tone Pad** | `shared/.../playback/EqualizerCurve.kt`, `playback/EqualizerProcessor.kt` | **10 fixed trapezoidal SVF slots** (`EqLayout.SLOTS = 10`): slots `0..6` manual (`60, 150, 400, 1000, 2500, 6000, 14000 Hz`; low-shelf at 60 Hz, high-shelf at 14 kHz, bells between, `Q = 1.0`, `±12 dB`), slots `7..9` tone pad (`250 Hz` low-shelf, `1 kHz` bell, `4 kHz` high-shelf, `±5` steps × `1.2 dB`). Inactive slots stay at `0 dB` (skipped, state cleared) — never rebuild cascade mid-stream. Coefficients glide every `GLIDE_FRAMES = 64` frames (`GLIDE_RATE = 0.08`, linear for dB/balance, geometric in `ln(Q)` for Q), flush denormals `< 1e-12`, clamp freq ≤ `0.45 * sr`. **50% partial makeup preamp** (`MAKEUP_FRACTION = 0.5`) computed *off the audio thread* across `96` log-spaced points `20 Hz..20 kHz`. Stereo L/R balance (`-1..1`) is an output trim (inactive on mono). |
| **Peak Limiter & Loudness Norm** | `shared/.../playback/PeakLimiter.kt`, `playback/LoudnessProcessor.kt` | **Per-deck** loudness normalization (not global session effect, so crossfade decks level each song independently). Glides gain/trim (`GLIDE_SECONDS = 0.04`). Instant-attack, channel-linked peak limiter at `CEILING = 0.985` (`RELEASE_SECONDS = 0.1` in `PeakLimiter`, `0.08` in `LoudnessProcessor`). Held active while `equalizer.isShaping` is true (catches the remaining 50% EQ boost overs). Parks to bit-exact bypass once settled (`reduction >= 0.97`, `|gain - 1| < 1e-4`). At a gapless boundary, `onStreamBoundary()` promotes `nextMediaId` at the exact sample boundary. |
| **Spatial Audio** | `playback/SpatialAudioProcessor.kt` | Stereo-only (`channelCount == 2`, silent pass-through on mono/multichannel — never error!). Mid/side widening (`widthGain = 2.5`, `outputGain = 0.82`) + `15 ms` delayed one-pole low-passed (`lowpassCoeff = 0.3`) crossfeed (`crossfeedGain = 0.2`). |
| **True Crossfade & Gapless** | `playback/CrossfadeController.kt`, `ui/screens/SettingsSheet.kt:798` | Slider `0..12 s` (`0` = gapless). **Dual-deck peer architecture**: active deck + standby deck preloaded with the **incoming** track (never duplicate outgoing audio onto standby, which caused 9–41 ms double-play seams). Equal-power `sin`/`cos` curve (`sin² + cos² = 1`). Metadata/queue handoff at `HANDOFF_AT = 0.5` (or `HANDOFF_GUARD_MS = 400 ms` before outgoing EOF). Smooth blend headroom trim `1 / sqrt(inGain + outGain)` floored at `MIN_TRIM = 0.5` (`-6 dB`) keeps equal-power sum ≤ `1.19` without limiter pumping. Seek/queue-replace triggers smooth bail ramp (`BAIL_STEP_MS = 15`). |
| **Skip Silence** | `playback/PlaybackService.kt:6785-6868,9561` | Media3's default `100 ms` threshold rushes songs; BitChord uses **`MIN_SILENCE_US = 1_000_000` (1.0 s)** to collapse track run-in/run-out dead air while preserving musical pauses/breaths. |
| **Playback Speed** | `PlaybackService.kt:7308`, `CrossfadeController.kt:646-654` | Pitch-preserving speed (`0.5x–2.0x`), must track slider drags immediately without gain-notch stalls; deferred while `crossfade.isTransitioning()` is true. |
| **Sleep Timer** | `playback/SleepTimer.kt`, `CrossfadeController.kt:255` | Monotonic clock deadline (`Instant`, never wall-clock), presets `15/30/45/60 min`, custom `5..720 min` in 5-min steps, plus `afterTrack` flag (pauses on track-end event, not computed from `duration - pos`). Uses `SLEEP_FADE` fade-out at track end. |
| **HTTP Streaming Pacing** | `playback/ChunkedDataSource.kt`, `PlaybackService.kt:9558` | `googlevideo` throttles unbounded HTTP GETs to `~15.5 kB/s`. Fetching bounded ranges of **`STREAM_CHUNK_BYTES = 1 MiB`** (capped by `clen` query parameter) achieves line rate (`~5.7 MB/s`). Must report HTTP 403/410 refusals back to `StreamResolver` and handle up to `MAX_EMPTY_RANGES = 3` truncated chunks. |
| **NerdStats Telemetry** | `shared/.../data/NerdStats.kt`, `PlaybackService.kt:5743-6037` | Strict **measured vs. claimed** separation with `TelemetryProvenance` (`AUTHORITATIVE`, `MEASURED`, `DERIVED`, `UNKNOWN`). `isLossless` (`flac`, `alac`, `raw`, `audio/dsd*`) is decided **solely by the decoder**, never `claimed`. `isHiRes` = `isLossless && (bitDepth > 16 || sampleRate > 48_000)`. Lossy tiers: Low `1..80 kbps`, Medium `81..255 kbps`, High `>= 256 kbps`. `pcmDataRateKbps` (`sr * depth * ch / 1000`) is never conflated with encoded bitrate. Guarded by atomic `generation` + `activeMediaId` so standby crossfade deck or stale async reads never overwrite the playing track. **Critical rule (`PlaybackService.kt:5750-5759`):** never infer `bitDepth = 32` from an `f32` carrier/DSP path — read source bit depth from the codec header (`STREAMINFO` / `bits_per_sample`). |
| **Downloads, Tagging & Local Library** | `download/{DownloadStore,MediaTagger,FlacTagger,Mp4Tagger,WebmTagger,LyricsTag}.kt`, `data/LocalMediaRepository.kt` | Save to `~/Music/BitChord` as sanitized `Artist - Title.ext` (`MAX_STEM_CHARS = 120`) via atomic `.part` rename. Cover art scaled to `≤ 1000 px` JPEG (q=92) + SHA-256 sidecar cache. Embeds dual lyrics: portable plain LRC (`LYRICS` / `©lyr`, cap `64,000` chars) **and** word-synced Enhanced LRC under **`BITCHORD_LYRICS`** (cap `128,000` chars; in MP4 as `----` freeform box with `mean = "com.music.bitchord"`, `name = "BITCHORD_LYRICS"`). Serialize rewrites & stream-copy FLAC frames to prevent `3×` file-size RAM spikes. Local scanner ignores tracks `< 30 s` and non-music folders (`/recordings/`, `/podcasts/`, etc.), scanning `mp3, m4a, mp4, flac, ogg, opus, aac, webm` (+ `dsf, dff` in v2). |

---

## 2. Recommended Rust Crate Stack (Exact Verified Versions)

All versions and MSRVs verified via `cargo info` / crates.io API on 2026-11-21 (host `rustc 1.100.0` satisfies every crate).

| Layer | Recommended Crate & Version | MSRV | License | Key Features / Config | Notes & Alternatives |
| --- | --- | --- | --- | --- | --- |
| **Core Demux + Decode** | `symphonia = "0.6.1"` | 1.85 | MPL-2.0 | `features = ["opt-simd", "flac", "aac", "alac", "mp3", "vorbis", "pcm", "adpcm", "isomp4", "mkv", "ogg", "wav", "aiff"]` | Pure Rust, non-owning packets, hardened demuxers, SIMD (`SSE`/`AVX`). **No native Opus!** |
| **Opus Decoder (Required!)** | `symphonia-adapter-libopus = "0.3.0"` | 1.89 | MIT/Apache-2.0 | `default-features = false` (links system `libopus 1.6.1`); enable `bundled` for portable builds | Wraps reference `libopus` into `symphonia-core ^0.6` (`814k` dl). Pure-Rust `symphonia-opus 0.1.3` (`1.7k` dl, 2026-11-15) is too young for v1 default. |
| **Optional FFmpeg Fallback** | `ffmpeg-next = "9.1.0"` *(or `ffmpeg-the-third = "6.0.0+ffmpeg-9.0"`)* | — / 1.80 | WTFPL | Feature-gated (`decoder-ffmpeg`), disabled by default in v1 | Both support host `FFmpeg 9.0.2` (`libavcodec 63`). Reserve for v2 HLS/DASH/HE-AAC/E-AC-3 to avoid `bindgen` build weight. |
| **Audio Output (v1 Primary)** | `pipewire = "0.10.1"` + `libspa = "0.10.1"` | 1.80 | MIT | `libspa` feature `v1_6_0` (matches host `1.6.11`) | Direct PipeWire stream: sets `media.role=Music`, app name, `node.rate`, cork events, zero double-buffer xruns. |
| **Audio Output (Fallback / Portable)** | `cpal = "0.18.2"` | 1.85 | Apache-2.0 | `features = ["pipewire", "realtime"]` *(optional backend behind `AudioSink` trait)* | Ships native PipeWire backend (`pipewire ^0.10`) with priority `PipeWire > PulseAudio > ALSA`, but pulls `alsa ^0.11` unconditionally and has 2-period xrun traps (see §6). |
| **Bit-Perfect Output (v2)** | `alsa = "0.12.1"` | — | Apache-2.0/MIT | Default `std` | Direct `hw:CARD=...,DEV=0` exclusive PCM + hardware sample-rate probing in v2. |
| **Lock-Free SPSC Ring Buffer** | `rtrb = "0.4.1"` *(or `ringbuf = "0.6.0"`)* | 1.38 / — | MIT/Apache-2.0 | Default `std` | Both wait-free SPSC. Prefer `rtrb 0.4.1` for RT sample/command queues (tiny, zero-copy `read_chunk`/`write_chunk`); use `ringbuf 0.6.0` if `StaticRb` / `LocalRb` is needed. |
| **Sample-Rate Conversion** | `rubato = "5.1.0"` | 1.87 | MIT/Apache-2.0 | Default `fft_resampler` | Uses `audioadapter 5.0` / `audioadapter-buffers 5.1`. `process_into_buffer()` is allocation-free. Used on decode thread so mixed-rate crossfade decks share one `f32` rate. |
| **Playback Speed (Time-Stretch)** | Pure-Rust Sonic/WSOLA *(v1)* or `signalsmith-stretch = "0.1.3"` *(opt-in)* | — | MIT / Apache-2.0 | Behind `TimeStretcher` trait | **Never use `rubato` for speed** (shifts pitch!). `signalsmith-stretch 0.1.3` (`207k` dl, `cc`+`bindgen`) gives studio polyphonic quality & aligns with v2 `native/loopmix`; pure-Rust Sonic (~400 LOC) has zero C++ build cost and matches Media3 parity. Avoid `soundtouch 0.5.4` (LGPL-2.1). |
| **Metadata Tagging** | `lofty = "0.25.5"` | 1.89 | MIT/Apache-2.0 | Default `id3v2_compression_support` | Supports FLAC, MP4/M4A (incl. `----` freeform atoms), Ogg Opus/Vorbis, MP3, WAV. **Does NOT support WebM/Matroska** (see §4). |
| **Local Library Scan + Index** | `walkdir = "2.5.1"`, `rusqlite = "0.43.0"` | — / 1.89 | MIT | `rusqlite = { version = "0.43.0", default-features = false, features = ["modern_sqlite", "cache"] }` | Links host `libsqlite3 3.54.1` cleanly (use `bundled` for release/AppImage). Enable `WAL` mode. |
| **D-Bus & MPRIS2** | `zbus = "6.5.0"`, `mpris-server = "0.11.0"` | 1.89 | MIT / MPL-2.0 | `mpris-server = { version = "0.11.0", features = ["tokio"] }` | Unified on `zbus 6`. |
| **System Tray (SNI)** | `ksni = "0.3.7"` | 1.80 | Unlicense | Default `tokio` (isolate in `bitchord-desktop`) | Implements `org.kde.StatusNotifierItem` + `dbusmenu`. **Note:** pulls `zbus ^5`; isolate to desktop crate. |
| **Secret Service / Credentials** | `oo7 = "0.7.3"` | 1.95 | MIT | Default `tokio, native_crypto` | Unified on **`zbus 6`**, pure-Rust crypto (no OpenSSL), auto-switches host `org.freedesktop.secrets` ↔ Flatpak Secret Portal. Prefer over `keyring 4.2.0` (which pulls `zbus ^5`). |
| **XDG Paths** | `directories = "6.0.0"` | — | MIT/Apache-2.0 | Default | `ProjectDirs` (`~/.config/bitchord`, `~/.local/share/bitchord`, `~/.cache/bitchord`) + `UserDirs::audio_dir()` (`~/Music/BitChord`). |

### Decoding Trade-Offs: Symphonia vs. FFmpeg vs. GStreamer

| Criterion | `symphonia 0.6.1` + `symphonia-adapter-libopus 0.3.0` | `ffmpeg-next 9.1.0` / `ffmpeg-the-third 6.0.0+ffmpeg-9.0` | `gstreamer 0.25.4` (`v1_28`) |
| --- | --- | --- | --- |
| **Codec Coverage** | FLAC, Vorbis, AAC-LC, MP3, ALAC, PCM, WAV, WebM/MKV, MP4/M4A + Opus via `libopus`. **Missing:** HE-AAC/SBR, E-AC-3 JOC (Atmos), HLS/DASH manifests. | Everything (HE-AAC, Opus, ALAC, FLAC, E-AC-3, HLS/DASH, DSD). | Everything via `gst-plugins-{base,good,bad,ugly,libav}` (runtime plugin dependency!). |
| **Memory & RT Control** | Full ownership of PCM frames, zero hidden threads, easy dual-deck gapless/crossfade, exact `CodecParameters` for `NerdStats`. | C `AVCodecContext` lifecycle; custom `AVIOContext` needed for BitChord's `ChunkedDataSource`. | Heavy GLib/GObject pipeline; fighting GStreamer's clock/gapless/crossfade abstractions defeats custom DSP. |
| **Build & ABI Risk (Arch)** | Zero `bindgen`; links stable `libopus.so.0`. Fast incremental builds (ADR-007). | `bindgen ^0.73` against system headers; Arch rolling FFmpeg bumps (e.g. `n9.0.2` / `libavcodec 63`) frequently break crates until updated. | Needs `gstreamer-audio`, `gstreamer-app`, `glib`, plus distro plugin packages at runtime. |
| **Verdict** | **v1 Primary.** Add `symphonia-adapter-fdk-aac 0.2.0` only if HE-AAC streams are needed (note FDK-AAC non-free license). | **Optional v2 / feature-gated fallback** behind `AudioDecoder` trait for HLS/DASH/HE-AAC/Atmos. | **Skip.** Unnecessary GLib/plugin complexity for a music player with custom Rust DSP. |

---

## 3. Audio Engine Architecture & Real-Time Thread Safety

### Thread Topology (Dual-Deck, Zero-Lock RT Path)

```text
┌────────────────────────────────────────────────────────────────────────────┐
│ Tokio Async Runtime (Control / Network / MPRIS / UI bridge)                │
│  • StreamResolver + ChunkedRangeFetcher (1 MiB HTTP Range → disk cache)    │
│  • CrossfadeCoordinator (30ms control ticks, EqualPower + BlendTrim calc)  │
│  • EqCurve preamp calculation (96-point log sweep off audio thread)        │
│  • Deferred Garbage Reaper (drops retired Tuning / RingBuf / Decks safely) │
└───────┬────────────────────────────────────────────┬───────────────────────┘
        │ ControlCmd (rtrb SPSC)                     │ AudioEvent / Telemetry
        ▼                                            │ (atomic / rtrb SPSC)
┌─────────────────────────────────────────────┐      │
│ Decode + Rate/Time Worker Thread(s)         │      │
│  Deck A (Active) & Deck B (Standby/Preload) │      │
│  1. Symphonia (+ libopus) → interleaved f32 │      │
│  2. SkipSilence (1.0s floor, 20ms xfade)    │      │
│  3. TimeStretcher (0.5x–2.0x pitch-safe)    │      │
│  4. Rubato 5.1.0 (only if track_sr != out)  │      │
│  5. StreamBoundary marker tagging           │      │
└───────┬─────────────────────────┬───────────┘      │
        │ Deck A f32 (rtrb SPSC)  │ Deck B f32 (rtrb SPSC)
        │ (~2–4s pre-buffered)    │ (~2–4s pre-buffered)
        ▼                         ▼                  │
┌────────────────────────────────────────────────────┴───────────────────────┐
│ HARD REAL-TIME AUDIO CALLBACK (PipeWire RT thread / CPAL callback)         │
│  • Pop N frames from Deck A (+ Deck B if FADING)                           │
│  • Per-deck DSP: Spatial → EQ (10-slot SVF, 64-frame glide) → Loudness+Lim │
│  • Mix: out = deck_a * cos(θ) + deck_b * sin(θ) (w/ blend trim)            │
│  • Update AtomicU64 played_frames + peak meter; zero-fill on underrun      │
└────────────────────────────────────────────────────────────────────────────┘
```

**Why split Decode/Resample/Stretch vs. RT Callback this way?**
- Decoding (`symphonia`, `libopus`), HTTP range I/O, FFT resampling (`rubato`), and phase-vocoder stretching have variable CPU spikes and frame latency; putting them on a dedicated worker thread feeding a `~2–4 s` `rtrb` SPSC ring buffer isolates the RT thread completely from I/O stalls and variable frame sizes.
- Keeping `Spatial` + `EQ` + `Loudness/PeakLimiter` + `Crossfade Mixer` inside the RT callback gives **~21 ms** (one `1024`-frame PipeWire quantum at `48 kHz`) UI responsiveness when dragging EQ sliders, volume, balance, or scrubbing/bailing a crossfade — while executing in bounded `O(N)` floating-point arithmetic with zero allocations.
- **Resampling on mixed-rate crossfades:** Normalizing each deck to the active PipeWire output rate (typically `48 kHz`) on the decode thread guarantees Deck A (e.g., `44.1 kHz` FLAC) and Deck B (e.g., `48 kHz` YouTube Opus) mix sample-accurately with zero rate mismatch and true gapless joins across different sample rates. When `track_sr == output_sr`, bypass `rubato` completely!

### Mandatory Real-Time Audio Thread Safety Rules

Any code reachable from the PipeWire `process` callback (or CPAL data callback) **MUST obey all of these invariants**:

1. **Zero Heap Allocations:** No `Vec::push`/`reserve`, `Box::new`, `String`, `format!`, `Arc::new`, `HashMap`, or `tokio` calls. Pre-allocate all DSP state (`AudioBlock`, SVF state arrays, `SpatialAudioProcessor` delay lines) when configuring the stream off-thread.
2. **Zero Heap Deallocations (The Drop Trap!):** Dropping a `Box`, `Vec`, `String`, or the last `Arc` / `rtrb::Consumer` inside the RT callback invokes `free()` (which takes allocator locks!). Never hold `Arc<EqCurve>` that could hit refcount 0 on the RT thread: pass fixed-size `Copy` structs (`#[repr(C)] struct TuningSnapshot { gains_db: [f32; 10], qs: [f32; 10], preamp_db: f32, balance: f32 }`) through an `rtrb::Producer<TuningSnapshot>` or a lock-free triple-buffer, and recycle any heap-owned objects back to the control thread via a dedicated `reclaim` SPSC queue.
3. **Zero Blocking Synchronization:** Never touch `std::sync::Mutex`, `RwLock`, `Condvar`, `parking_lot`, `tokio::sync`, `channel::recv`, or spin-locks. Use only wait-free SPSC (`rtrb`/`ringbuf`) and lock-free atomics (`AtomicU64`, `AtomicU32`, `AtomicBool`, `Ordering::Acquire`/`Release`/`Relaxed`).
4. **Zero System Calls & Zero Logging:** Never call `println!`, `eprintln!`, `log::*`, `tracing::*`, file I/O, `Instant::now()` (prefer audio frame counter + PipeWire `spa_io_clock`), or `thread::sleep`. Increment atomic counters (`underrun_count`, `callback_overruns`) and log them from the control thread.
5. **No Panics / Unwinds:** Use saturating/clamped indexing or slice iterators; never `.unwrap()` on fallible ring buffer reads. On ring-buffer underrun (network stall), write `0.0f32` silence and increment an atomic xrun counter.
6. **Prevent Denormal CPU Spikes:** Flush SVF integrator states `< 1e-12` to `0.0` per sub-block (matching `EqualizerProcessor.kt:399-408`), because x86 Intel CPUs suffer 10–100× slowdowns on subnormal float operations during silence.
7. **PipeWire Thread Affinity:** `libpipewire` objects (`MainLoop`, `Context`, `Core`, `Stream`) are `!Send + !Sync`. Run `pipewire::main_loop::MainLoop` on its own dedicated OS thread and send lifecycle commands (`Connect`, `SetActive`, `ChangeRate`, `Quit`) via `pipewire::channel::channel()`.
8. **Dev-Profile Optimization (ADR-007):** Debug builds (`opt-level = 0`) make `symphonia` and `rubato` ~15–20× slower and cause audible xruns. Pin `opt-level = 2` selectively in workspace `Cargo.toml` for audio crates:
   ```toml
   [profile.dev.package.symphonia-codec-aac] opt-level = 2
   [profile.dev.package.symphonia-bundle-flac] opt-level = 2
   [profile.dev.package.symphonia-bundle-mp3] opt-level = 2
   [profile.dev.package.symphonia-codec-vorbis] opt-level = 2
   [profile.dev.package.symphonia-codec-alac] opt-level = 2
   [profile.dev.package.rubato] opt-level = 2
   [profile.dev.package.bitchord-dsp-core] opt-level = 2
   ```

---

## 4. Streaming, Tagging, Local Library & Desktop Services Design

### HTTP Range Streaming (`ChunkedRangeSource` for Symphonia)
- Implement `symphonia::core::io::MediaSource` (`Read + Seek + Send + Sync`) backed by a sparse disk/temp cache file + background Tokio fetcher.
- Replicate `ChunkedDataSource.kt`: when the URL carries `clen` (all progressive `googlevideo` streams), issue bounded `Range: bytes=start-end` requests of **`1 MiB`** (`1_048_576` bytes) instead of an open-ended GET. Verify HTTP `206 Partial Content` (a `200 OK` response on a ranged seek must NOT be misread as the requested offset!).
- `stream-download 0.24.6` can bootstrap prototyping, **but** its default `reqwest` fetcher issues unbounded GETs from offset 0 — to bypass `googlevideo` pacing you must either implement its custom `http::Client` trait with `1 MiB` chunking or write a focused ~250 LOC `ChunkedCacheSource` directly (which also integrates cleanly with BitChord's persistent disk `AudioCache` and `StreamResolver::on_playback_refused(url, status)`).

### Metadata Tagging & Local Library (`lofty`, `walkdir`, `rusqlite`)
- **`lofty 0.25.5`** handles reading/writing FLAC (`VorbisComments` + `Picture`), M4A/MP4 (`Ilst` including `AtomIdent::Freeform { mean: "com.music.bitchord", name: "BITCHORD_LYRICS" }` and `©lyr`), Ogg Opus/Vorbis, MP3 (`ID3v2`), and WAV.
- **WebM/Matroska Gap:** `lofty 0.25.5` **cannot read or write `.webm`/`.mkv`**.
  - For **downloads**: match upstream `DownloadStore.kt:34,139-144` — download AAC/ALAC as `.m4a` and FLAC as `.flac`; if saving YouTube Opus offline, either remux raw Opus packets from WebM into **Ogg `.opus`** (trivially taggable by `lofty` and universally supported by Linux players) OR port BitChord's ~150 LOC `WebmTagger.kt` EBML tail writer.
  - For **local library scanning**: run `lofty::read_from_path` first; on `.webm`/`.mkv` (or unsupported container), fall back to `symphonia`'s `mkv` demuxer metadata reader so existing `.webm` files in `~/Music` still show title/artist/duration and embedded `BITCHORD_LYRICS`.
- **FLAC Memory Safety:** `lofty` writes FLAC metadata in-place when `PADDING` suffices; when growing headers on large hi-res FLACs, serialize tagging behind a Tokio `Semaphore(1)` (matching `MediaTagger.kt:121` `taggingLock`) and write via atomic `.part` + `rename` within the same filesystem.
- **SQLite (`rusqlite 0.43.0`):** Use `PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;` with `walkdir 2.5.1` (`follow_links(false)`), indexing `(path, mtime_secs, size_bytes)` for incremental rescans so unchanged tracks skip `lofty` I/O.

### Linux Desktop OS Integration (`zbus`, `mpris-server`, `ksni`, `oo7`, `directories`)
- **MPRIS2 (`mpris-server 0.11.0` + `zbus 6.5.0`):**
  - Bus name: `org.mpris.MediaPlayer2.bitchord` (fallback `.instance<pid>` if another instance holds it), object path `/org/mpris/MediaPlayer2`, `DesktopEntry = "bitchord"`.
  - Expose `RootInterface` (`Raise`, `Quit`, `Identity = "BitChord"`) and `PlayerInterface` (`Play`, `Pause`, `PlayPause`, `Next`, `Previous`, `Seek`, `SetPosition`, `Volume`, `LoopStatus`, `Shuffle`, `Rate`, `Metadata`, `PlaybackStatus`, `Position`).
- **System Tray (`ksni 0.3.7`):**
  - Verified live on host: `cutefish-compos` owns `org.kde.StatusNotifierWatcher` and `org.kde.StatusNotifierHost-869`. Works identically on KDE Plasma and GNOME (with AppIndicator extension).
  - Always treat tray spawn failure as **non-fatal** (vanilla GNOME without AppIndicator has no `StatusNotifierWatcher`).
  - Isolate `ksni` inside `crates/bitchord-desktop` so its `zbus ^5` dependency never leaks into core headless crates.
- **Credentials (`oo7 0.7.3`):**
  - Use `oo7::Keyring::new().await` — automatically connects to `org.freedesktop.secrets` (`gnome-keyring-daemon` on host) natively and switches to `org.freedesktop.portal.Secret` encrypted file store inside Flatpak sandboxes.
  - Handle `Locked` / unavailable keyring gracefully (e.g., headless CLI or locked login keyring) with a clear error/prompt rather than panicking.
- **XDG Directories (`directories 6.0.0`):**
  - `ProjectDirs::from("com.music", "BitChord", "bitchord")`: config `$XDG_CONFIG_HOME/bitchord`, data/SQLite `$XDG_DATA_HOME/bitchord`, audio/artwork cache `$XDG_CACHE_HOME/bitchord`.
  - Downloads export folder: `UserDirs::new().and_then(|u| u.audio_dir()).unwrap_or(home/Music).join("BitChord")`.

---

## 5. Semantic Traps & Platform Gotchas

| Area | Trap / Failure Mode | Root Cause | Concrete Mitigation |
| --- | --- | --- | --- |
| **Symphonia Opus** | `Unsupported("codec")` on every YouTube Music track (`itag 251` WebM/Opus). | `symphonia 0.6.1` `features = ["all"]` has **no built-in Opus decoder**. | Register `symphonia_adapter_libopus::OpusDecoder` in a custom `CodecRegistry` (with `default-features = false` to link system `libopus 1.6.1`). |
| **Symphonia AAC** | Silent decode failure or half-rate audio on low-bitrate YouTube `mp4a.40.5` (HE-AAC). | `symphonia-codec-aac 0.6.1` supports **AAC-LC only** (no SBR/PS). | Prefer `Opus` (itag 251) or `AAC-LC` (`mp4a.40.2`, itag 140/141) in `StreamResolver`; log codec string explicitly on decode error; add FFmpeg/FDK-AAC fallback only if needed. |
| **Symphonia HTTP Seeking** | Demuxer hangs, loops, or restarts track at `0:00` on seek. | Returning `is_seekable() = true` while `byte_len() = None`, or server answering `200 OK` instead of `206`, or WebM lacking front `Cues` / MP4 lacking `sidx` (fragmented MP4). | Always supply `byte_len = Some(clen)` when known; assert `206 Partial Content`; on unindexed WebM/fMP4 without `Cues`/`sidx`, fall back to byte-estimate or buffered progressive seek; replicate `Mp4Sidx` indexing on `.m4a` downloads. |
| **Opus Pre-Skip & Gain** | Audible ~6.5 ms click/gap at start of Opus tracks or wrong loudness. | Opus RFC 7845 / WebM `CodecDelay` requires discarding initial `pre_skip` (typically 312 samples at 48 kHz) and honoring header output gain. | Verify `symphonia-adapter-libopus` applies `CodecParameters.delay` / `padding` and discard leading delay frames on track start & seek. |
| **Time-Stretch vs. Resample** | Playback speed `1.5x` sounds like chipmunks. | Confusing `rubato` (sample-rate conversion, shifts pitch if used for speed) with time-stretching. | Use `rubato 5.1.0` **only** for `track_sr → output_sr` conversion; use pitch-preserving WSOLA/Sonic or `signalsmith-stretch 0.1.3` for `playback_speed != 1.0`. Account for stretcher algorithmic latency (`input_latency + output_latency`) in playhead/lyrics timestamps! |
| **PipeWire Resampling & Bit-Perfect** | Playing a `44.1 kHz` or `96 kHz` FLAC silently resamples to `48 kHz`. | Measured on host: `pw-metadata -n settings` sets `clock.allowed-rates = [ 48000 ]`. PipeWire locks the graph to 48 kHz regardless of client `node.rate`. | In v1, report `system_mixer_rate_hz = 48000` honestly in `AudioOutputStatus` (never claim bit-perfect over default PipeWire!). In v2, support `alsa 0.12.1` exclusive `hw:` mode + document `~/.config/pipewire/pipewire.conf.d/` `default.clock.allowed-rates` for PipeWire Pro Audio users. |
| **CPAL 0.18 Double-Buffering Xruns** | Crackling/xruns even with trivial callback on PipeWire/ALSA. | CPAL `0.17+` switched to strict 2-period buffering; non-power-of-two `BufferSize::Fixed` or mismatched SR triggers period misalignment. | Prefer direct `pipewire 0.10.1` in v1; if using `cpal 0.18.2`, always use power-of-two buffer sizes (`512`/`1024`) and match the host's default `48000 Hz` rate. |
| **NerdStats Bit-Depth Lie** | Plain 16-bit/44.1 kHz FLAC displays "Hi-Res Lossless (32-bit)". | Reading bit depth from internal `f32` DSP/output buffer instead of the decoder's source stream header. | Read `track.codec_params.bits_per_sample` (and FLAC `STREAMINFO`), never the `f32` pipeline format; guard `publish()` with `(generation, active_media_id)` so standby crossfade deck never overwrites telemetry. |
| **EQ/Limiter Clipping & Bypass** | Bass boost clips or turning EQ off leaves permanent `-0.2 dB` attenuation. | `EqCurve` only applies `50%` makeup attenuation; naive bypass cuts filter state abruptly causing a click. | Glide `enabled = false` toward `EqCurve::FLAT`, hold `LoudnessProcessor` limiter active while `eq.is_shaping`, and park limiter to exact unity once `reduction >= 0.97` (`LoudnessProcessor.kt:157`). |
| **`zbus` v5 vs v6 Split** | Compiling two full copies of `zbus` + async runtimes increases RAM & build time (ADR-007). | `mpris-server 0.11` and `oo7 0.7.3` use **`zbus ^6`**, while `ksni 0.3.7` and `keyring 4.2.0` still use **`zbus ^5`**. | Choose `oo7 0.7.3` over `keyring 4.2.0` (eliminates one `zbus 5` user and gains portal support), and isolate `ksni 0.3.7` strictly in `bitchord-desktop`. |
| **MPRIS2 Spec Traps** | Scrub bar freezes, jumps, or D-Bus rejects metadata with `InvalidArgs`. | (1) Emitting `PropertiesChanged` for `Position` floods D-Bus and breaks clients; (2) putting raw YouTube IDs like `-Abc_123` into `mpris:trackid` violates D-Bus `ObjectPath` rules (`[A-Za-z0-9_/]` only, no `-`!); (3) remote `mpris:artUrl` fails offline/expired. | Never emit `PropertiesChanged` for `Position` — emit `Signal::Seeked { position }` on seeks/discontinuities and let clients extrapolate via `Rate`. Encode `trackid` as `/com/music/bitchord/track/<hex_or_seq>`. Point `mpris:artUrl` to cached local `file://` JPEG. |
| **`ksni` Tray Icon Colors** | Tray icon renders blue/inverted on Cutefish/KDE. | SNI `icon_pixmap` requires **ARGB32 in big-endian (network) byte order**, whereas `image::RgbaImage` is RGBA byte order. | Convert every pixel as `((a as u32) << 24 \| (r as u32) << 16 \| (g as u32) << 8 \| b as u32).to_be_bytes()`. |
| **`lofty` WebM / MP4 Custom Tags** | Crash/error tagging `.webm` downloads or losing word-synced lyrics on `.m4a`. | `lofty 0.25.5` has no WebM/Matroska backend; plain `©lyr` shouldn't contain `<mm:ss.xx>` A2 word tags (other players render raw timestamps). | Save downloads as `.flac`, `.m4a`, or Ogg `.opus` (or keep EBML `WebmTagger`); store plain LRC in `LYRICS`/`©lyr` and Enhanced LRC in `BITCHORD_LYRICS` (MP4 freeform `mean="com.music.bitchord"`). |
| **Skip Silence Position Drift** | Synced lyrics drift behind/ahead of vocals when Skip Silence is enabled. | Dropping silent frames shortens wall-clock duration relative to raw decoded sample counts. | Advance the track playhead from the **source media timestamp** attached to the frame actually exiting the speaker (accounting for ring-buffer latency), never from raw output frames rendered. |

---

## 6. Recommended `Cargo.toml` Dependency Blueprint (Wave 3)

```toml
# crates/bitchord-audio/Cargo.toml
[dependencies]
bitchord-dsp-core = { path = "../bitchord-dsp-core" }
bitchord-model = { path = "../bitchord-model" }

symphonia = { version = "0.6.1", default-features = false, features = [
    "opt-simd", "flac", "aac", "alac", "mp3", "vorbis", "pcm", "adpcm",
    "isomp4", "mkv", "ogg", "wav", "aiff", "all-meta",
] }
symphonia-adapter-libopus = { version = "0.3.0", default-features = false }
pipewire = "0.10.1"
libspa = { version = "0.10.1", features = ["v1_6_0"] }
rtrb = "0.4.1"
rubato = "5.1.0"
mpris-server = { version = "0.11.0", features = ["tokio"] }
zbus = { version = "6.5.0", default-features = false, features = ["tokio"] }

[features]
default = []
bundled-opus = ["symphonia-adapter-libopus/bundled"]
cpal-fallback = ["dep:cpal"]
stretch-signalsmith = ["dep:signalsmith-stretch"]
decoder-ffmpeg = ["dep:ffmpeg-next"]

[dependencies.cpal]
version = "0.18.2"
optional = true
features = ["pipewire", "realtime"]

[dependencies.signalsmith-stretch]
version = "0.1.3"
optional = true

[dependencies.ffmpeg-next]
version = "9.1.0"
optional = true
default-features = false
features = ["codec", "format", "software-resampling"]
```

```toml
# crates/bitchord-storage/Cargo.toml (Wave 2/3)
[dependencies]
lofty = "0.25.5"
walkdir = "2.5.1"
rusqlite = { version = "0.43.0", default-features = false, features = ["modern_sqlite", "cache"] }
oo7 = "0.7.3"
directories = "6.0.0"

# crates/bitchord-desktop/Cargo.toml (Wave 4 — isolated from workspace default-members)
[dependencies]
ksni = "0.3.7"
```
