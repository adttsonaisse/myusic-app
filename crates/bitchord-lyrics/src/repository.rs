use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;

use crate::http::LyricsHttp;
use crate::providers::{
    BetterLyrics, BiniHit, BiniLyrics, Genius, KuGou, LrcLib, LrcRed, LyricsPlus, Megalobiz,
    Musixmatch, PaxSenix, SimpMusicLyrics, Unison, YouTubeLyricsBackend, YouTubeMusicLyrics,
    YouTubeTranscriptLyrics,
};
use crate::{
    LyricLine, LyricsSource, artist_for_lyrics_search, for_lyrics_search, with_background_vocals,
};

const IDENTIFY_TIMEOUT_MS: u64 = 2_500;
const REMEMBERED_ISRCS: usize = 100;

/// Winning lyrics and the provider that supplied them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricsResult {
    pub source: LyricsSource,
    pub lines: Vec<LyricLine>,
}

/// Parameters for a [`LyricsRepository::lyrics`] lookup.
#[derive(Debug, Clone)]
pub struct LyricsRequest<'a> {
    pub video_id: &'a str,
    pub title: &'a str,
    pub artist: &'a str,
    pub duration_ms: i64,
    pub album: Option<&'a str>,
    pub sources: HashSet<LyricsSource>,
    pub order: Vec<LyricsSource>,
    pub prioritize_syllable_sync: bool,
    pub isrc: Option<&'a str>,
}

impl<'a> LyricsRequest<'a> {
    pub fn new(video_id: &'a str, title: &'a str, artist: &'a str, duration_ms: i64) -> Self {
        Self {
            video_id,
            title,
            artist,
            duration_ms,
            album: None,
            sources: LyricsSource::ALL.iter().copied().collect(),
            order: LyricsSource::ALL.to_vec(),
            prioritize_syllable_sync: false,
            isrc: None,
        }
    }
}

pub type SourceCallback = Arc<dyn Fn(LyricsSource) + Send + Sync>;
pub type SourceResultCallback = Arc<dyn Fn(LyricsSource, Option<&LyricsResult>) + Send + Sync>;

/// Optional progress callbacks matching upstream `LyricsRepository.lyrics`.
#[derive(Clone, Default)]
pub struct LyricsCallbacks {
    pub on_source_started: Option<SourceCallback>,
    pub on_source_result: Option<SourceResultCallback>,
    pub on_source_cancelled: Option<SourceCallback>,
}

/// Bounded in-memory LRU cache (`video_id -> isrc`).
#[derive(Debug, Default)]
struct IsrcLru {
    entries: HashMap<String, String>,
    order: VecDeque<String>,
}

impl IsrcLru {
    fn get(&mut self, key: &str) -> Option<String> {
        let value = self.entries.get(key)?.clone();
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.to_owned());
        Some(value)
    }

    fn insert(&mut self, key: String, value: String) {
        if self.entries.contains_key(&key) {
            if let Some(pos) = self.order.iter().position(|k| k == &key) {
                self.order.remove(pos);
            }
        } else if self.entries.len() >= REMEMBERED_ISRCS
            && let Some(oldest) = self.order.pop_front()
        {
            self.entries.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, value);
    }
}

/// Deduplicates URL fetches across sources within a single lookup so `LrcRed`
/// and `BiniLyrics` share one download when they ask for the same TTML URL.
struct SharedDocuments {
    http: LyricsHttp,
    in_flight: Mutex<HashMap<String, SharedFetch>>,
}

struct SharedFetch {
    rx: watch::Receiver<Option<Option<String>>>,
    handle: JoinHandle<()>,
}

impl SharedDocuments {
    fn new(http: LyricsHttp) -> Self {
        Self {
            http,
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    async fn get(&self, url: String) -> Option<String> {
        let mut rx = {
            let mut map = self.in_flight.lock().await;
            if let Some(existing) = map.get(&url) {
                existing.rx.clone()
            } else {
                let (tx, rx) = watch::channel(None);
                let http = self.http.clone();
                let fetch_url = url.clone();
                let handle = tokio::spawn(async move {
                    let body = http.get(&fetch_url).await;
                    let _ = tx.send(Some(body));
                });
                map.insert(
                    url,
                    SharedFetch {
                        rx: rx.clone(),
                        handle,
                    },
                );
                rx
            }
        };

        loop {
            if let Some(result) = rx.borrow().clone() {
                return result;
            }
            if rx.changed().await.is_err() {
                return None;
            }
        }
    }

    async fn cancel(&self) {
        let mut map = self.in_flight.lock().await;
        for (_, fetch) in map.drain() {
            fetch.handle.abort();
        }
    }
}

/// Orchestrates all enabled lyrics providers concurrently, awaiting results in
/// strict user priority order while deduplicating shared ISRC/TTML downloads
/// and keeping `Genius` lazy.
pub struct LyricsRepository {
    http: LyricsHttp,
    pax_senix: Arc<PaxSenix>,
    lyrics_plus: Arc<LyricsPlus>,
    musixmatch: Arc<Musixmatch>,
    youtube: Option<Arc<dyn YouTubeLyricsBackend>>,
    isrcs: Arc<StdMutex<IsrcLru>>,
}

impl Default for LyricsRepository {
    fn default() -> Self {
        Self::new()
    }
}

impl LyricsRepository {
    pub fn new() -> Self {
        Self {
            http: LyricsHttp::new(),
            pax_senix: Arc::new(PaxSenix::new()),
            lyrics_plus: Arc::new(LyricsPlus::new()),
            musixmatch: Arc::new(Musixmatch::new()),
            youtube: None,
            isrcs: Arc::new(StdMutex::new(IsrcLru::default())),
        }
    }

    pub fn with_http(mut self, http: LyricsHttp) -> Self {
        self.http = http;
        self
    }

    pub fn with_youtube_backend(mut self, backend: Arc<dyn YouTubeLyricsBackend>) -> Self {
        self.youtube = Some(backend);
        self
    }

    pub fn set_paxsenix_api_key(&self, key: &str) {
        self.pax_senix.set_api_key(key);
    }

    pub fn http(&self) -> &LyricsHttp {
        &self.http
    }

    fn known_isrc(&self, video_id: &str) -> Option<String> {
        if video_id.is_empty() {
            return None;
        }
        self.isrcs.lock().ok()?.get(video_id)
    }

    fn remember_isrc(&self, video_id: &str, isrc: Option<&str>) {
        let Some(code) = isrc.filter(|s| !s.trim().is_empty()) else {
            return;
        };
        if video_id.is_empty() {
            return;
        }
        if let Ok(mut cache) = self.isrcs.lock() {
            cache.insert(video_id.to_owned(), code.to_owned());
        }
    }

    /// Races every enabled non-hidden provider concurrently and returns the
    /// highest-priority acceptable match, cancelling slower race losers.
    pub async fn lyrics(
        &self,
        request: &LyricsRequest<'_>,
        callbacks: &LyricsCallbacks,
    ) -> Option<LyricsResult> {
        let sequence = build_sequence(&request.order, &request.sources);
        if sequence.is_empty() {
            return None;
        }

        let search_title = for_lyrics_search(request.title);
        let search_artist = artist_for_lyrics_search(request.artist);

        let known = request
            .isrc
            .filter(|s| !s.trim().is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| self.known_isrc(request.video_id));

        let hit = if known.is_none() && sequence.contains(&LyricsSource::BiniLyrics) {
            let identified = tokio::time::timeout(
                Duration::from_millis(IDENTIFY_TIMEOUT_MS),
                BiniLyrics::identify(
                    &self.http,
                    &search_title,
                    &search_artist,
                    request.duration_ms,
                    request.album,
                    None,
                ),
            )
            .await
            .ok()
            .flatten();
            if let Some(ref h) = identified {
                self.remember_isrc(request.video_id, h.isrc.as_deref());
            }
            identified
        } else {
            None
        };

        let recording = known.or_else(|| {
            hit.as_ref()
                .and_then(|h| h.isrc.as_deref())
                .filter(|s| !s.trim().is_empty())
                .map(ToOwned::to_owned)
        });
        let hit = hit.map(Arc::new);
        let documents = Arc::new(SharedDocuments::new(self.http.clone()));

        struct RaceSlot {
            source: LyricsSource,
            completed: Arc<AtomicBool>,
            handle: Option<JoinHandle<Option<LyricsResult>>>,
        }

        let mut racing: Vec<RaceSlot> = Vec::with_capacity(sequence.len());
        for &source in &sequence {
            let completed = Arc::new(AtomicBool::new(false));
            if source == LyricsSource::Genius {
                // Lazy: only spawned if every higher-priority source misses.
                racing.push(RaceSlot {
                    source,
                    completed,
                    handle: None,
                });
                continue;
            }
            let handle = self.spawn_source(
                source,
                request.video_id.to_owned(),
                search_title.clone(),
                search_artist.clone(),
                request.duration_ms,
                request.album.map(ToOwned::to_owned),
                recording.clone(),
                hit.clone(),
                Arc::clone(&documents),
                callbacks.clone(),
                Arc::clone(&completed),
            );
            racing.push(RaceSlot {
                source,
                completed,
                handle: Some(handle),
            });
        }

        let mut line_synced: Option<LyricsResult> = None;
        let mut winner: Option<LyricsResult> = None;

        for slot in &mut racing {
            let source = slot.source;
            if line_synced.is_some() && source == LyricsSource::Genius {
                continue;
            }

            let handle = match slot.handle.take() {
                Some(h) => h,
                None => self.spawn_source(
                    source,
                    request.video_id.to_owned(),
                    search_title.clone(),
                    search_artist.clone(),
                    request.duration_ms,
                    request.album.map(ToOwned::to_owned),
                    recording.clone(),
                    hit.clone(),
                    Arc::clone(&documents),
                    callbacks.clone(),
                    Arc::clone(&slot.completed),
                ),
            };

            let Ok(Some(found)) = handle.await else {
                continue;
            };
            if found.lines.iter().any(LyricLine::is_word_synced) {
                winner = Some(found);
                break;
            }
            if !request.prioritize_syllable_sync && found.lines.iter().any(|l| l.time_ms > 0) {
                winner = Some(found);
                break;
            }
            if line_synced.is_none() {
                line_synced = Some(found);
            }
        }

        for slot in &mut racing {
            if let Some(handle) = slot.handle.take() {
                handle.abort();
                if !slot.completed.load(Ordering::Acquire)
                    && let Some(ref cb) = callbacks.on_source_cancelled
                {
                    cb(slot.source);
                }
            }
        }
        documents.cancel().await;

        winner.or(line_synced)
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_source(
        &self,
        source: LyricsSource,
        video_id: String,
        title: String,
        artist: String,
        duration_ms: i64,
        album: Option<String>,
        recording: Option<String>,
        hit: Option<Arc<BiniHit>>,
        documents: Arc<SharedDocuments>,
        callbacks: LyricsCallbacks,
        completed: Arc<AtomicBool>,
    ) -> JoinHandle<Option<LyricsResult>> {
        let http = self.http.clone();
        let pax_senix = Arc::clone(&self.pax_senix);
        let lyrics_plus = Arc::clone(&self.lyrics_plus);
        let musixmatch = Arc::clone(&self.musixmatch);
        let youtube = self.youtube.clone();
        let isrcs = Arc::clone(&self.isrcs);

        tokio::spawn(async move {
            if let Some(ref cb) = callbacks.on_source_started {
                cb(source);
            }

            let lines = match source {
                LyricsSource::LrcRed => {
                    let docs = Arc::clone(&documents);
                    LrcRed::lyrics(
                        &http,
                        &title,
                        &artist,
                        duration_ms,
                        recording.as_deref(),
                        move |url| {
                            let docs = Arc::clone(&docs);
                            async move { docs.get(url).await }
                        },
                    )
                    .await
                }
                LyricsSource::BetterLyrics => {
                    BetterLyrics::lyrics(&http, &title, &artist, duration_ms, album.as_deref())
                        .await
                }
                LyricsSource::BetterLyricsPortato => {
                    BetterLyrics::portato(&http, &title, &artist, duration_ms, album.as_deref())
                        .await
                }
                LyricsSource::LyricsPlus => {
                    lyrics_plus
                        .lyrics(
                            &http,
                            &title,
                            &artist,
                            duration_ms,
                            album.as_deref(),
                            recording.as_deref(),
                        )
                        .await
                }
                LyricsSource::BiniLyrics => {
                    let matched = if let Some(ref h) = hit {
                        let docs = Arc::clone(&documents);
                        BiniLyrics::lyrics_for(h, move |url| {
                            let docs = Arc::clone(&docs);
                            async move { docs.get(url).await }
                        })
                        .await
                    } else {
                        let docs = Arc::clone(&documents);
                        BiniLyrics::lyrics(
                            &http,
                            &title,
                            &artist,
                            duration_ms,
                            album.as_deref(),
                            recording.as_deref(),
                            move |url| {
                                let docs = Arc::clone(&docs);
                                async move { docs.get(url).await }
                            },
                        )
                        .await
                    };
                    if let Some(m) = matched {
                        if let Some(code) = m.isrc.as_deref().filter(|s| !s.trim().is_empty())
                            && !video_id.is_empty()
                            && let Ok(mut cache) = isrcs.lock()
                        {
                            cache.insert(video_id.clone(), code.to_owned());
                        }
                        Some(m.lines)
                    } else {
                        None
                    }
                }
                LyricsSource::Unison => {
                    Unison::lyrics(&http, &title, &artist, duration_ms, album.as_deref()).await
                }
                LyricsSource::SimpMusic => {
                    SimpMusicLyrics::lyrics(&http, &video_id, duration_ms).await
                }
                LyricsSource::YoutubeTranscript => match youtube.as_deref() {
                    Some(backend) => YouTubeTranscriptLyrics::lyrics(backend, &video_id).await,
                    None => None,
                },
                LyricsSource::YoutubeMusic => match youtube.as_deref() {
                    Some(backend) => YouTubeMusicLyrics::lyrics(backend, &video_id).await,
                    None => None,
                },
                LyricsSource::LrcLib => LrcLib::lyrics(&http, &title, &artist, duration_ms).await,
                LyricsSource::Musixmatch => {
                    musixmatch.lyrics(&http, &title, &artist, duration_ms).await
                }
                LyricsSource::PaxSenix => {
                    pax_senix
                        .lyrics(&http, &title, &artist, duration_ms, album.as_deref())
                        .await
                }
                LyricsSource::PaxSenixSpotify => {
                    pax_senix
                        .spotify_lyrics(&http, &title, &artist, duration_ms)
                        .await
                }
                LyricsSource::PaxSenixMusixmatch => {
                    pax_senix
                        .musixmatch_lyrics(&http, &title, &artist, duration_ms)
                        .await
                }
                LyricsSource::KuGou => {
                    KuGou::lyrics(&http, &title, &artist, duration_ms, album.as_deref()).await
                }
                LyricsSource::Megalobiz => Megalobiz::lyrics(&http, &title, &artist).await,
                LyricsSource::Genius => Genius::lyrics(&http, &title, &artist).await,
            };

            let result = lines.map(|raw| LyricsResult {
                source,
                lines: with_background_vocals(raw),
            });
            completed.store(true, Ordering::Release);
            if let Some(ref cb) = callbacks.on_source_result {
                cb(source, result.as_ref());
            }
            result
        })
    }
}

fn build_sequence(order: &[LyricsSource], sources: &HashSet<LyricsSource>) -> Vec<LyricsSource> {
    let mut seen = HashSet::new();
    let mut seq = Vec::new();
    for &src in order.iter().chain(LyricsSource::OFFERED.iter()) {
        if seen.insert(src) && sources.contains(&src) && !src.hidden() {
            seq.push(src);
        }
    }
    seq
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::BoxFuture;
    use serde_json::{Value, json};
    use std::sync::atomic::AtomicUsize;

    struct SlowYoutubeBackend {
        music_delay_ms: u64,
        transcript_delay_ms: u64,
    }

    impl YouTubeLyricsBackend for SlowYoutubeBackend {
        fn next<'a>(&'a self, _video_id: &'a str) -> BoxFuture<'a, Option<Value>> {
            let delay = self.music_delay_ms;
            Box::pin(async move {
                if delay > 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                Some(json!({
                    "tabs": [{
                        "tabRenderer": {
                            "title": "Lyrics",
                            "endpoint": { "browseEndpoint": { "browseId": "MPLYt_1" } }
                        }
                    }]
                }))
            })
        }

        fn browse<'a>(
            &'a self,
            _browse_id: &'a str,
            _params: Option<&'a str>,
        ) -> BoxFuture<'a, Option<Value>> {
            Box::pin(async {
                Some(json!({
                    "musicDescriptionShelfRenderer": {
                        "description": { "runs": [{ "text": "Plain line (echo)" }] }
                    }
                }))
            })
        }

        fn transcript<'a>(&'a self, _video_id: &'a str) -> BoxFuture<'a, Option<Value>> {
            let delay = self.transcript_delay_ms;
            Box::pin(async move {
                if delay > 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                Some(json!({
                    "cues": [
                        { "transcriptCueRenderer": { "startOffsetMs": "1500", "cue": { "simpleText": "Timed line (ooh)" } } }
                    ]
                }))
            })
        }
    }

    #[tokio::test]
    async fn respects_priority_order_applies_background_vocals_and_cancels_losers() {
        let repo = LyricsRepository::new().with_youtube_backend(Arc::new(SlowYoutubeBackend {
            music_delay_ms: 10,
            transcript_delay_ms: 60,
        }));

        let cancelled = Arc::new(AtomicUsize::new(0));
        let cancelled_ref = Arc::clone(&cancelled);
        let callbacks = LyricsCallbacks {
            on_source_cancelled: Some(Arc::new(move |_| {
                cancelled_ref.fetch_add(1, Ordering::SeqCst);
            })),
            ..Default::default()
        };

        let mut req = LyricsRequest::new("dQw4w9WgXcQ", "Test", "Artist", 180_000);
        req.sources = [LyricsSource::YoutubeTranscript, LyricsSource::YoutubeMusic]
            .into_iter()
            .collect();
        req.order = vec![LyricsSource::YoutubeTranscript, LyricsSource::YoutubeMusic];

        let found = repo.lyrics(&req, &callbacks).await.expect("should resolve");
        // YoutubeTranscript has priority and timed lines (>0), so it wins even
        // though YoutubeMusic finished earlier.
        assert_eq!(found.source, LyricsSource::YoutubeTranscript);
        assert_eq!(found.lines[0].text, "Timed line");
        assert_eq!(
            found.lines[0]
                .background
                .as_deref()
                .map(|b| b.text.as_str()),
            Some("(ooh)")
        );
        // YoutubeMusic already finished before Transcript, so 0 cancelled here.
        assert_eq!(cancelled.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cancels_slower_lower_priority_source_when_higher_priority_wins() {
        let repo = LyricsRepository::new().with_youtube_backend(Arc::new(SlowYoutubeBackend {
            music_delay_ms: 500,
            transcript_delay_ms: 10,
        }));

        let cancelled = Arc::new(AtomicUsize::new(0));
        let cancelled_ref = Arc::clone(&cancelled);
        let callbacks = LyricsCallbacks {
            on_source_cancelled: Some(Arc::new(move |_| {
                cancelled_ref.fetch_add(1, Ordering::SeqCst);
            })),
            ..Default::default()
        };

        let mut req = LyricsRequest::new("dQw4w9WgXcQ", "Test", "Artist", 180_000);
        req.sources = [LyricsSource::YoutubeTranscript, LyricsSource::YoutubeMusic]
            .into_iter()
            .collect();
        req.order = vec![LyricsSource::YoutubeTranscript, LyricsSource::YoutubeMusic];

        let found = repo.lyrics(&req, &callbacks).await.expect("should resolve");
        assert_eq!(found.source, LyricsSource::YoutubeTranscript);
        assert_eq!(cancelled.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn skips_hidden_sources_and_empty_source_set_returns_none() {
        let repo = LyricsRepository::new();
        let mut req = LyricsRequest::new("dQw4w9WgXcQ", "Test", "Artist", 180_000);
        req.sources = [LyricsSource::SimpMusic, LyricsSource::Megalobiz]
            .into_iter()
            .collect();
        req.order = vec![LyricsSource::SimpMusic, LyricsSource::Megalobiz];
        assert!(
            repo.lyrics(&req, &LyricsCallbacks::default())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn shared_documents_deduplicates_concurrent_fetches() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_srv = Arc::clone(&hits);

        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let hits_srv = Arc::clone(&hits_srv);
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf).await;
                    hits_srv.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    let body = "hello ttml";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });

        let docs = Arc::new(SharedDocuments::new(LyricsHttp::new()));
        let url = format!("http://{addr}/s/GBUM71029604.ttml");
        let d1 = Arc::clone(&docs);
        let d2 = Arc::clone(&docs);
        let u1 = url.clone();
        let u2 = url.clone();
        let (r1, r2) = tokio::join!(d1.get(u1), d2.get(u2));
        assert_eq!(r1.as_deref(), Some("hello ttml"));
        assert_eq!(r2.as_deref(), Some("hello ttml"));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
