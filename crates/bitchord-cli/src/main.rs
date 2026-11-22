#![forbid(unsafe_code)]

//! Headless CLI front-door and deterministic replay harness for BitChord Linux Desktop.
//!
//! Exercises `bitchord-innertube`, `bitchord-lyrics`, `bitchord-integrations`,
//! `bitchord-storage`, `bitchord-queue`, and `bitchord-jam` end-to-end without
//! a GUI. Pass `--json` on any command for stable machine-readable output.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use bitchord_innertube::{
    ContainerPreference, InnertubeClient, InnertubeParser, NativeStreamBackend, ResolveRequest,
    SongInfoRepository, StreamBackend, StreamResolver, UploadedTracks, YtMusicRepository,
    build_shared_http_client,
};
#[cfg(feature = "ytdlp-bridge")]
use bitchord_innertube::{FallbackStreamBackend, YtDlpStreamBackend};
use bitchord_integrations::{
    ListenTogetherClient, SpotifyImporter, parse_and_normalize_server_url,
};
use bitchord_jam::parse_invite_url;
use bitchord_lyrics::{
    LyricsCallbacks, LyricsRepository, LyricsRequest, LyricsSource, to_enhanced_lrc, to_lrc,
};
use bitchord_model::{PlaybackSourceType, QueueTier, SearchFilter, SearchResult, Song};
use bitchord_queue::{QueueSource, QueueTimeline};
use bitchord_storage::{AppPaths, EmbeddedLyrics, LibraryDb, SettingsStore, read_local_audio_tags};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "bitchord-cli",
    version,
    about = "Headless CLI front-door for BitChord Linux Desktop"
)]
struct Cli {
    /// Emit machine-readable JSON output for scripting and behavioral replay.
    #[arg(long, global = true)]
    json: bool,

    /// Override the XDG root directory (isolated config/data/cache tree).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Search YouTube Music via native InnerTube.
    Search(SearchArgs),
    /// Browse an album, playlist, or podcast by browseId.
    Browse(BrowseArgs),
    /// Inspect an artist page by browseId.
    Artist {
        /// YouTube Music artist channel browseId (e.g. UC...).
        browse_id: String,
    },
    /// Fetch extended watch-page metadata and engagement counts for a videoId.
    SongInfo {
        video_id: String,
        #[arg(long, default_value = "")]
        title: String,
        #[arg(long, default_value = "")]
        artist: String,
    },
    /// Race lyrics providers (including native YouTube Music / transcript backend).
    Lyrics(LyricsArgs),
    /// Resolve a playable audio stream URL (native-first with optional yt-dlp bridge).
    Resolve(ResolveArgs),
    /// Listen Together party server utilities and invite parsing.
    Jam {
        #[command(subcommand)]
        action: JamAction,
    },
    /// Spotify playlist URL parsing and public playlist track fetching.
    Spotify {
        #[command(subcommand)]
        action: SpotifyAction,
    },
    /// Inspect XDG paths, SQLite stats, downloads, local tags, and embedded lyrics.
    Storage {
        #[command(subcommand)]
        action: StorageAction,
    },
    /// Deterministic queue state machine smoke command for replay verification.
    QueueDemo {
        /// Comma-separated video IDs.
        #[arg(long, value_delimiter = ',')]
        tracks: Vec<String>,
        /// Optional index of the track to insert as "Play Next" (UserQueue tier).
        #[arg(long)]
        play_next: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliSearchFilter {
    All,
    Songs,
    Videos,
    Albums,
    Artists,
    Playlists,
}

impl From<CliSearchFilter> for SearchFilter {
    fn from(value: CliSearchFilter) -> Self {
        match value {
            CliSearchFilter::All => Self::All,
            CliSearchFilter::Songs => Self::Songs,
            CliSearchFilter::Videos => Self::Videos,
            CliSearchFilter::Albums => Self::Albums,
            CliSearchFilter::Artists => Self::Artists,
            CliSearchFilter::Playlists => Self::Playlists,
        }
    }
}

#[derive(Debug, Args)]
struct SearchArgs {
    query: String,
    #[arg(long, value_enum, default_value_t = CliSearchFilter::Songs)]
    filter: CliSearchFilter,
    #[arg(long, default_value_t = 10)]
    limit: usize,
    #[arg(long)]
    anonymous: bool,
}

#[derive(Debug, Args)]
struct BrowseArgs {
    browse_id: String,
    #[arg(long)]
    params: Option<String>,
}

#[derive(Debug, Args)]
struct LyricsArgs {
    #[arg(long)]
    title: String,
    #[arg(long)]
    artist: String,
    #[arg(long, default_value = "")]
    video_id: String,
    #[arg(long, default_value_t = 0)]
    duration_ms: i64,
    #[arg(long)]
    album: Option<String>,
    /// Query only a single LyricsSource by wire name (e.g. LRCLIB, BETTER_LYRICS, LRC_RED).
    #[arg(long)]
    source: Option<String>,
    #[arg(long)]
    syllable_first: bool,
    /// Print raw LRC output (enhanced A2 if word timings are present).
    #[arg(long)]
    lrc: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum StreamBackendMode {
    Auto,
    Native,
    #[cfg(feature = "ytdlp-bridge")]
    Ytdlp,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliContainer {
    Any,
    Webm,
    M4a,
}

impl From<CliContainer> for ContainerPreference {
    fn from(value: CliContainer) -> Self {
        match value {
            CliContainer::Any => Self::Any,
            CliContainer::Webm => Self::WebmOpus,
            CliContainer::M4a => Self::M4aAac,
        }
    }
}

#[derive(Debug, Args)]
struct ResolveArgs {
    video_id: String,
    #[arg(long, default_value_t = u32::MAX)]
    max_kbps: u32,
    #[arg(long, value_enum, default_value_t = StreamBackendMode::Auto)]
    backend: StreamBackendMode,
    #[arg(long, value_enum, default_value_t = CliContainer::Any)]
    container: CliContainer,
    /// Skip the 1 MiB + 16 KiB range probe (diagnostic only).
    #[arg(long)]
    no_probe: bool,
}

#[derive(Debug, Subcommand)]
enum JamAction {
    /// Probe GET /healthz on a Listen Together server.
    Health {
        #[arg(long)]
        server: String,
        #[arg(long, default_value_t = 6_000)]
        timeout_ms: u64,
    },
    /// Fetch GET /api/time from a Listen Together server.
    Time {
        #[arg(long)]
        server: String,
    },
    /// Preview an active party by 6-character code.
    Preview {
        #[arg(long)]
        server: String,
        #[arg(long)]
        code: String,
    },
    /// Parse a Listen Together invite link or code offline.
    ParseInvite { input: String },
    /// Validate and normalize a custom Listen Together server URL offline.
    NormalizeUrl { url: String },
}

#[derive(Debug, Subcommand)]
enum SpotifyAction {
    /// Extract a 22-character playlist ID from a Spotify URL or URI offline.
    Parse { input: String },
    /// Fetch public playlist title and tracks via Spotify's embed endpoint.
    Fetch { input: String },
}

#[derive(Debug, Subcommand)]
enum StorageAction {
    /// Print resolved Linux XDG paths for config, data, cache, and downloads.
    Paths,
    /// Open SQLite library database and report row statistics.
    Stats,
    /// List saved download records from SQLite.
    Downloads,
    /// Read embedded or sidecar offline lyrics from an audio or `.m3u8` file.
    EmbeddedLyrics { path: PathBuf },
    /// Inspect local audio file metadata tags and stream properties via `lofty`.
    Tags { path: PathBuf },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputMessage<T: Serialize> {
    ok: bool,
    data: T,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(out) => {
            if !out.is_empty() {
                println!("{out}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<String, String> {
    let paths = match &cli.data_dir {
        Some(root) => AppPaths::in_root(root),
        None => AppPaths::resolve(),
    };
    let json_mode = cli.json;

    match cli.command {
        Command::Search(args) => cmd_search(args, json_mode).await,
        Command::Browse(args) => cmd_browse(args, json_mode).await,
        Command::Artist { browse_id } => cmd_artist(&browse_id, json_mode).await,
        Command::SongInfo {
            video_id,
            title,
            artist,
        } => cmd_song_info(&video_id, &title, &artist, json_mode).await,
        Command::Lyrics(args) => cmd_lyrics(args, json_mode).await,
        Command::Resolve(args) => cmd_resolve(args, json_mode).await,
        Command::Jam { action } => cmd_jam(action, json_mode).await,
        Command::Spotify { action } => cmd_spotify(action, json_mode).await,
        Command::Storage { action } => cmd_storage(action, &paths, json_mode),
        Command::QueueDemo { tracks, play_next } => cmd_queue_demo(tracks, play_next, json_mode),
    }
}

fn build_yt_repo() -> (Arc<InnertubeClient>, YtMusicRepository) {
    let http = Arc::new(build_shared_http_client());
    let client = Arc::new(InnertubeClient::with_http_client(http));
    let parser = Arc::new(InnertubeParser::default());
    let repo = YtMusicRepository::new(Arc::clone(&client), parser);
    (client, repo)
}

async fn cmd_search(args: SearchArgs, json_mode: bool) -> Result<String, String> {
    let (_, repo) = build_yt_repo();
    let mut page = repo
        .search(&args.query, args.filter.into(), args.anonymous)
        .await
        .map_err(|e| e.to_string())?;
    page.rows.truncate(args.limit);

    if json_mode {
        return to_json(&page.rows);
    }
    let mut lines = Vec::new();
    for (idx, row) in page.rows.iter().enumerate() {
        match row {
            SearchResult::TopTrack { song } | SearchResult::Track { song } => {
                lines.push(format!(
                    "{:>2}. [{}] {} — {} ({})",
                    idx + 1,
                    song.video_id,
                    song.title,
                    song.artist,
                    song.duration_text.as_deref().unwrap_or("?")
                ));
            }
            SearchResult::Browse { item } => {
                lines.push(format!(
                    "{:>2}. [{:?}:{}] {} — {}",
                    idx + 1,
                    item.r#type,
                    item.browse_id,
                    item.title,
                    item.subtitle
                ));
            }
        }
    }
    Ok(lines.join("\n"))
}

async fn cmd_browse(args: BrowseArgs, json_mode: bool) -> Result<String, String> {
    let (_, repo) = build_yt_repo();
    let (header, songs, continuation) = repo
        .browse_songs(&args.browse_id, args.params.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    if json_mode {
        return to_json(&serde_json::json!({
            "header": header,
            "songs": songs,
            "continuation": continuation,
        }));
    }
    let mut out = Vec::new();
    if let Some(h) = header {
        out.push(format!("{} — {}", h.title, h.subtitle));
    }
    for (idx, s) in songs.iter().enumerate() {
        out.push(format!(
            "{:>2}. [{}] {} — {}",
            idx + 1,
            s.video_id,
            s.title,
            s.artist
        ));
    }
    Ok(out.join("\n"))
}

async fn cmd_artist(browse_id: &str, json_mode: bool) -> Result<String, String> {
    let (_, repo) = build_yt_repo();
    let page = repo
        .artist_page(browse_id)
        .await
        .map_err(|e| e.to_string())?;
    if json_mode {
        return to_json(&page);
    }
    Ok(format!(
        "{} ({})\nSongs: {} | Sections: {}",
        page.name.as_deref().unwrap_or("Unknown Artist"),
        page.subscriber_count_text
            .as_deref()
            .unwrap_or("subscribers unknown"),
        page.songs.len(),
        page.sections.len()
    ))
}

async fn cmd_song_info(
    video_id: &str,
    title: &str,
    artist: &str,
    json_mode: bool,
) -> Result<String, String> {
    let http = Arc::new(build_shared_http_client());
    let client = InnertubeClient::with_http_client(http);
    let parser = InnertubeParser::default();
    let song = Song::new(video_id, title, artist, None);
    let info = SongInfoRepository::load(&client, &parser, &song).await;
    if json_mode {
        return to_json(&info);
    }
    Ok(format!(
        "album={} year={} date={} views={} likes={} comments={} failed={}",
        info.album.as_deref().unwrap_or("-"),
        info.release_year.as_deref().unwrap_or("-"),
        info.release_date.as_deref().unwrap_or("-"),
        info.views.as_deref().unwrap_or("-"),
        info.likes.as_deref().unwrap_or("-"),
        info.comments.as_deref().unwrap_or("-"),
        info.failed
    ))
}

async fn cmd_lyrics(args: LyricsArgs, json_mode: bool) -> Result<String, String> {
    let (client, _) = build_yt_repo();
    let repo = LyricsRepository::new().with_youtube_backend(client);
    let mut req = LyricsRequest::new(&args.video_id, &args.title, &args.artist, args.duration_ms);
    req.album = args.album.as_deref();
    req.prioritize_syllable_sync = args.syllable_first;
    if let Some(ref src_name) = args.source {
        let normalized = src_name.trim().to_ascii_uppercase();
        let source = LyricsSource::from_wire_name(&normalized)
            .ok_or_else(|| format!("unknown lyrics source '{src_name}'"))?;
        req.sources = HashSet::from([source]);
        req.order = vec![source];
    }

    let Some(result) = repo.lyrics(&req, &LyricsCallbacks::default()).await else {
        return Err("no lyrics found".to_owned());
    };
    if json_mode {
        return to_json(&serde_json::json!({
            "source": result.source,
            "lineCount": result.lines.len(),
            "lines": result.lines,
        }));
    }
    if args.lrc {
        let enhanced = to_enhanced_lrc(&result.lines);
        if !enhanced.is_empty() {
            return Ok(enhanced);
        }
        return Ok(to_lrc(&result.lines));
    }
    let mut out = vec![format!(
        "Source: {} ({} lines)",
        result.source.label(),
        result.lines.len()
    )];
    for line in &result.lines {
        out.push(format!("[{:>6}ms] {}", line.time_ms, line.text));
    }
    Ok(out.join("\n"))
}

async fn cmd_resolve(args: ResolveArgs, json_mode: bool) -> Result<String, String> {
    let http = Arc::new(build_shared_http_client());
    let client = Arc::new(InnertubeClient::with_http_client(Arc::clone(&http)));
    let uploads = Arc::new(UploadedTracks::default());

    let backend: Arc<dyn StreamBackend> = match args.backend {
        StreamBackendMode::Native => Arc::new(NativeStreamBackend::new(client)),
        #[cfg(feature = "ytdlp-bridge")]
        StreamBackendMode::Ytdlp => Arc::new(YtDlpStreamBackend::new()),
        StreamBackendMode::Auto => {
            #[cfg(feature = "ytdlp-bridge")]
            {
                Arc::new(FallbackStreamBackend::new(
                    NativeStreamBackend::new(client),
                    YtDlpStreamBackend::new(),
                ))
            }
            #[cfg(not(feature = "ytdlp-bridge"))]
            {
                Arc::new(NativeStreamBackend::new(client))
            }
        }
    };

    let resolver = StreamResolver::new(http, backend, uploads).with_probing(!args.no_probe);
    let mut req = ResolveRequest::for_playback(&args.video_id, args.max_kbps);
    req.container = args.container.into();
    let stream = resolver.resolve(req).await.map_err(|e| e.to_string())?;
    if json_mode {
        return to_json(&stream);
    }
    Ok(format!(
        "videoId={}\nclient={} profile={}\nformat={} ({}) {} kbps\nurl={}",
        stream.video_id,
        stream.client_name,
        stream.profile_id,
        stream.ext,
        stream.mime_type,
        stream.kbps,
        stream.url
    ))
}

async fn cmd_jam(action: JamAction, json_mode: bool) -> Result<String, String> {
    match action {
        JamAction::ParseInvite { input } => {
            let parsed = parse_invite_url(Some(&input));
            if json_mode {
                return to_json(&serde_json::json!({
                    "input": input,
                    "code": parsed,
                }));
            }
            match parsed {
                Some(code) => Ok(code),
                None => Err("not a valid Listen Together invite URL or code".to_owned()),
            }
        }
        JamAction::NormalizeUrl { url } => {
            let normalized = parse_and_normalize_server_url(&url).map_err(|e| e.to_string())?;
            if json_mode {
                return to_json(&OutputMessage {
                    ok: true,
                    data: normalized,
                });
            }
            Ok(normalized)
        }
        JamAction::Health { server, timeout_ms } => {
            let client = ListenTogetherClient::new(reqwest::Client::new(), &server)
                .map_err(|e| e.to_string())?;
            let probe = client.healthz(Duration::from_millis(timeout_ms)).await;
            if json_mode {
                return to_json(&serde_json::json!({
                    "server": client.base_url(),
                    "isOnline": probe.is_online,
                    "latencyMs": probe.latency_ms,
                }));
            }
            Ok(format!(
                "{} online={} latency={}ms",
                client.base_url(),
                probe.is_online,
                probe.latency_ms
            ))
        }
        JamAction::Time { server } => {
            let client = ListenTogetherClient::new(reqwest::Client::new(), &server)
                .map_err(|e| e.to_string())?;
            let server_ms = client.server_time().await.map_err(|e| e.to_string())?;
            if json_mode {
                return to_json(&serde_json::json!({ "serverMs": server_ms }));
            }
            Ok(server_ms.to_string())
        }
        JamAction::Preview { server, code } => {
            let client = ListenTogetherClient::new(reqwest::Client::new(), &server)
                .map_err(|e| e.to_string())?;
            let preview = client
                .preview_party(&code)
                .await
                .map_err(|e| e.to_string())?;
            if json_mode {
                return to_json(&preview);
            }
            Ok(format!(
                "Party {} host={} members={}/{} full={}",
                preview.code,
                preview.host_name,
                preview.member_count,
                preview.max_members,
                preview.is_full
            ))
        }
    }
}

async fn cmd_spotify(action: SpotifyAction, json_mode: bool) -> Result<String, String> {
    match action {
        SpotifyAction::Parse { input } => {
            let id = SpotifyImporter::extract_playlist_id(&input);
            if json_mode {
                return to_json(&serde_json::json!({
                    "input": input,
                    "playlistId": id,
                }));
            }
            id.ok_or_else(|| "not a valid Spotify playlist URL or URI".to_owned())
        }
        SpotifyAction::Fetch { input } => {
            let playlist_id = SpotifyImporter::extract_playlist_id(&input).unwrap_or(input);
            let http = build_shared_http_client();
            let (title, tracks) = SpotifyImporter::fetch_playlist_tracks(&http, &playlist_id)
                .await
                .map_err(|e| e.to_string())?;
            if json_mode {
                return to_json(&serde_json::json!({
                    "playlistId": playlist_id,
                    "title": title,
                    "trackCount": tracks.len(),
                    "tracks": tracks,
                }));
            }
            let mut lines = vec![format!("{title} ({} tracks)", tracks.len())];
            for (idx, t) in tracks.iter().enumerate() {
                lines.push(format!("{:>3}. {} — {}", idx + 1, t.title, t.artist));
            }
            Ok(lines.join("\n"))
        }
    }
}

fn cmd_storage(action: StorageAction, paths: &AppPaths, json_mode: bool) -> Result<String, String> {
    match action {
        StorageAction::Paths => {
            if json_mode {
                return to_json(paths);
            }
            Ok(format!(
                "config: {}\ndata: {}\ncache: {}\ndownloads(private): {}\ndownloads(exported): {}",
                paths.config_dir.display(),
                paths.data_dir.display(),
                paths.cache_dir.display(),
                paths.downloads_private_dir.display(),
                paths.downloads_exported_dir.display(),
            ))
        }
        StorageAction::Stats => {
            let settings = SettingsStore::new(paths.settings_file())
                .load()
                .map_err(|e| e.to_string())?;
            let db = LibraryDb::open(&paths.db_file()).map_err(|e| e.to_string())?;
            let stats = db.stats().map_err(|e| e.to_string())?;
            if json_mode {
                return to_json(&serde_json::json!({
                    "dbPath": paths.db_file(),
                    "settingsPath": paths.settings_file(),
                    "downloadQuality": settings.download_quality,
                    "stats": stats,
                }));
            }
            Ok(format!(
                "songs={} playlists={} searchHistory={} plays={} downloads={} collections={}",
                stats.songs,
                stats.playlists,
                stats.search_history,
                stats.listening_history,
                stats.downloads,
                stats.downloaded_collections,
            ))
        }
        StorageAction::Downloads => {
            let db = LibraryDb::open(&paths.db_file()).map_err(|e| e.to_string())?;
            let rows = db.list_downloads().map_err(|e| e.to_string())?;
            if json_mode {
                return to_json(&rows);
            }
            if rows.is_empty() {
                return Ok("No downloads recorded.".to_owned());
            }
            Ok(rows
                .iter()
                .map(|r| format!("[{}] {} — {} ({})", r.video_id, r.artist, r.title, r.uri))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        StorageAction::EmbeddedLyrics { path } => {
            let Some(lines) = EmbeddedLyrics::for_path(&path) else {
                return Err(format!(
                    "no embedded or sidecar lyrics found in {}",
                    path.display()
                ));
            };
            if json_mode {
                return to_json(&lines);
            }
            Ok(to_lrc(&lines))
        }
        StorageAction::Tags { path } => {
            let Some(tags) = read_local_audio_tags(&path) else {
                return Err(format!("could not read audio tags from {}", path.display()));
            };
            if json_mode {
                return to_json(&tags);
            }
            Ok(format!(
                "title={} artist={} album={} duration={}ms sampleRate={:?} bitDepth={:?} channels={:?} bitrate={:?}kbps",
                tags.title.as_deref().unwrap_or("-"),
                tags.artist.as_deref().unwrap_or("-"),
                tags.album.as_deref().unwrap_or("-"),
                tags.duration_ms,
                tags.sample_rate_hz,
                tags.bit_depth,
                tags.channels,
                tags.audio_bitrate_kbps,
            ))
        }
    }
}

fn cmd_queue_demo(
    tracks: Vec<String>,
    play_next: Option<String>,
    json_mode: bool,
) -> Result<String, String> {
    if tracks.is_empty() {
        return Err("provide at least one track ID via --tracks".to_owned());
    }
    let songs: Vec<Song> = tracks
        .into_iter()
        .map(|id| Song::new(id.clone(), format!("Track {id}"), "Artist", None))
        .collect();
    let source = QueueSource {
        title: "CLI Demo".to_owned(),
        r#type: PlaybackSourceType::Queue,
        id: None,
    };
    let mut ctx = QueueTimeline::build_context_queue(&[], -1, &songs, 0, &source);
    if let Some(next_id) = play_next {
        let next_song = QueueTimeline::as_queue_entry(
            Song::new(next_id.clone(), format!("Track {next_id}"), "Artist", None),
            QueueTier::UserQueue,
        );
        let idx = QueueTimeline::find_user_queue_insertion_index(
            &ctx.timeline,
            ctx.start_index as isize,
            true,
        );
        ctx.timeline.insert(idx, next_song);
    }
    let ordered: Vec<&str> = ctx.timeline.iter().map(|s| s.video_id.as_str()).collect();
    if json_mode {
        return to_json(&serde_json::json!({
            "currentIndex": ctx.start_index,
            "order": ordered,
        }));
    }
    Ok(format!(
        "current={} order={}",
        ctx.start_index,
        ordered.join(" -> ")
    ))
}

fn to_json<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn offline_spotify_parse_and_jam_commands_emit_valid_json() {
        let cli = Cli::try_parse_from([
            "bitchord-cli",
            "--json",
            "spotify",
            "parse",
            "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M?si=xyz",
        ])
        .expect("parse cli");
        let out = run(cli).await.expect("run spotify parse");
        let val: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(val["playlistId"], "37i9dQZF1DXcBWIGoYBM5M");

        let cli = Cli::try_parse_from([
            "bitchord-cli",
            "--json",
            "jam",
            "normalize-url",
            "API.Example.COM/",
        ])
        .expect("parse jam normalize");
        let out = run(cli).await.expect("run jam normalize");
        let val: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(val["data"], "https://api.example.com");
    }

    #[tokio::test]
    async fn offline_storage_commands_respect_isolated_data_dir() {
        let root = std::env::temp_dir().join(format!("bitchord-cli-test-{}", std::process::id()));
        let root_str = root.to_string_lossy().into_owned();

        let cli = Cli::try_parse_from([
            "bitchord-cli",
            "--json",
            "--data-dir",
            &root_str,
            "storage",
            "paths",
        ])
        .expect("parse storage paths");
        let out = run(cli).await.expect("run storage paths");
        let val: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert!(
            val["dataDir"]
                .as_str()
                .unwrap()
                .contains("bitchord-cli-test")
        );

        let cli = Cli::try_parse_from([
            "bitchord-cli",
            "--json",
            "--data-dir",
            &root_str,
            "storage",
            "stats",
        ])
        .expect("parse storage stats");
        let out = run(cli).await.expect("run storage stats");
        let val: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(val["stats"]["songs"], 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn offline_queue_demo_inserts_play_next_at_index_one() {
        let cli = Cli::try_parse_from([
            "bitchord-cli",
            "--json",
            "queue-demo",
            "--tracks",
            "a,b,c",
            "--play-next",
            "u1",
        ])
        .expect("parse queue-demo");
        let out = run(cli).await.expect("run queue-demo");
        let val: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(val["currentIndex"], 0);
        assert_eq!(val["order"], serde_json::json!(["a", "u1", "b", "c"]));
    }
}
