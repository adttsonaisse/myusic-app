# BitChord Domain Glossary (Kotlin → Rust)

Maps upstream Kotlin `:shared` / `:app` domain terminology to consistent Rust crate, module, and type names across all waves. Add new terms whenever a wave introduces them.

| Upstream Kotlin Term / Package | Meaning | Rust Crate / Module / Name | Notes |
| --- | --- | --- | --- |
| `com.music.bitchord.data.model.Song` | Playable track entity (YT Music, local file, download, external source) | `bitchord_model::Song` | Uses `Option<String>` / `Vec<ArtistRef>`, `#[serde(rename_all = "camelCase")]` where serialized |
| `QueueTier` (`USER_QUEUE`, `CONTEXT`, `AUTOPLAY`) | Ownership tier of a track in playback queue | `bitchord_model::QueueTier` (`UserQueue`, `Context`, `Autoplay`) | Preserved across queue transforms |
| `ArtistRef` / `ArtistCredit` / `CreditSegment` | Credited artist with optional channel `browse_id` + styled credit line slicing | `bitchord_model::{ArtistRef, ArtistCredit, CreditSegment}` | `normalized_artist_name` uses Unicode NFD + strip combining marks + Unicode lowercase |
| `PlaylistOrder` | Deterministic ordering rules for playlists | `bitchord_model::playlist_order` | Pure sorting/reordering utilities |
| `VersionTimeMap` | Maps playback timestamp between audio version & music-video version | `bitchord_model::version_time_map::VersionTimeMap` | Prevents jump desync when switching audio/video |
| `SpotifyMatch` | Matched Spotify track/playlist reference | `bitchord_model::spotify_match` | Used by Spotify importer |
| `TopPicks` | Home feed quick-picks ranking/selection | `bitchord_model::top_picks` | Pure ranking logic |
| `SearchHistoryEntity` | Persisted search query item | `bitchord_model::SearchHistoryEntry` | Stored in XDG local SQLite/JSON |
| `LibraryEnums` / `PlayerEnums` / `AutomixPerformanceMode` / `TrackAnalysisState` | User settings enums for library sort, repeat/shuffle, quality, automix | `bitchord_model::settings::*` | Serializable to XDG config |
| `TrackMatcher` (`data/sources/TrackMatcher.kt`) | Cross-catalogue recording identity judgment (`TitleParts`, `Target`, `Candidate`, `queries`, `match`) | `bitchord_matcher::TrackMatcher` | Splits title into `words`, `versions`, `context`; kana→romaji support |
| `StreamFormat` / `SourceTrackKeys` | Audio quality/container tier & canonical source key | `bitchord_matcher::{StreamFormat, SourceTrackKeys}` | Ranks `Lossless` > `High` > `Low` |
| `PartyTrack`, `PartyMember`, `PartyPlayback`, `PartyQueue` | Listen Together Go server wire protocol DTOs | `bitchord_jam::protocol::*` | **Strict `camelCase` JSON** matching `backend/README.md` |
| `ServerClock` | Monotonic NTP-style client↔server clock offset estimator (lowest-RTT sample wins) | `bitchord_jam::clock::ServerClock` | Computes `party_position_ms` from `(position_ms, anchor_ms, is_playing)` |
| `JamInvite` | 6-character party invite code normalization (`O/I/L`→`0/1`) & invite link parser | `bitchord_jam::invite` | Matches Go `backend/codes/codes.go` |
| `PartyQueueReading` | Reading & diffing separate `PartyQueue` vs `PartyPlayback.queue_seq` | `bitchord_jam::queue_reading` | Self-heals stale queue copies |
| `EqualizerCurve` / `EqualizerPreset` | 10-band graphic EQ gain curves & built-in presets | `bitchord_dsp_core::eq::{EqualizerCurve, EqualizerPreset}` | Frequency bands & dB interpolation |
| `PeakLimiter` | Soft-knee lookahead/instant audio peak limiter preventing clipping | `bitchord_dsp_core::limiter::PeakLimiter` | Pure `f32` PCM processing |
| `KizzyRPC` / `DiscordWebSocket` (`com.my.kizzy`) | Discord Rich Presence Gateway WebSocket protocol & payload entities | `bitchord_integrations::*` | Native Linux: supports both local Discord IPC socket & Gateway RPC (Wave 2) |
| `LyricLine` / `SyllableRuns` / `TtmlLyrics` / `EnhancedLrc` | Word/syllable-timed lyrics representation & parsers | `bitchord_lyrics::*` | Done in Wave 1 — Unicode scalar indices (`chars().count()`), `quick-xml 0.42`, `Box<LyricLine>` for recursive `background`/`timing_source` |
| `LyricsRepository` / `SharedDocuments` / `YouTubeLyricsBackend` | Concurrent priority-ordered lyrics racing across 17 `LyricsSource` providers with ISRC lookup & lazy Genius | `bitchord_lyrics::{LyricsRepository, YouTubeLyricsBackend}` | Done in Wave 1 — trait seam avoids circular dep with Wave 2 `bitchord-innertube` |
| `LyricsTranslation` / `TranslationLanguages` | Batched Google Translate + Input Tools Latin-script retry + romanization + 2 MiB gzipped cache + 133 language table | `bitchord_lyrics::{LyricsTranslator, TRANSLATION_LANGUAGES}` | Done in Wave 1 — preserves `timing_source` across translated word sweeps |
| `QueueBuilder` / `QueueTimeline` / `QueueWindow` / `ContextRegistry` | Playback queue state machine, deterministic shuffle, continuation registry, and tier-pinned user queue | `bitchord_queue::*` | Done in Wave 1 |
| `TransitionFilter` | 4th-order Butterworth SVF DJ high-pass/low-pass crossfade filter | `bitchord_dsp_core::transition_filter::TransitionFilter` | Done in Wave 1 |
| `CanvasArtwork` / `SpotifyCanvasQuery` | Canvas animated artwork metadata matching & Spotify GraphQL persisted-query parser | `bitchord_model::canvas::*` | Done in Wave 1 |
| `Presence` / `PresencePlatform` | Live-install anonymous heartbeat (`POST /api/presence`, no `Origin` header, 2s close debounce, `close_blocking`) | `bitchord_integrations::presence::*` | Done in Wave 1 |
| `Innertube` / `StreamResolver` / `PoTokenGenerator` | YouTube Music API client & playable stream URL resolution | `bitchord_innertube::*` | Wave 2 — native-first + `yt-dlp` dev bridge (ADR-004) |
| `AudioEngine` (replaces ExoPlayer/Media3) | Gapless + crossfade Linux audio pipeline (`symphonia` + PipeWire/CPAL/ALSA) | `bitchord_audio::*` | Wave 3 |
| `BitChordApp` (replaces Compose `sharedUi` + `app/ui`) | GPUI (`gpui-ce 0.3.x`, ADR-009) desktop window, views, mesh gradient, lyrics canvas | `bitchord_desktop::*` | Wave 4 |
