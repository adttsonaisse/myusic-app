use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use bitchord_model::Song;
use serde::{Deserialize, Serialize};

/// Canonical identity of a row within a browsed collection.
/// Preserves duplicate playlist entries distinguished by `set_video_id`.
pub fn context_entry_key(song: &Song) -> String {
    match song
        .set_video_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    {
        Some(set_id) => format!("{}@{set_id}", song.video_id),
        None => song.video_id.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContinuationState {
    Available,
    InFlight,
    Exhausted,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContinuationDiagnostic {
    DuplicatePage,
    NoProgress,
    RepeatedContinuation,
    RequestFailed,
}

/// Immutable read-only snapshot of the active browsed context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSnapshot {
    pub context_token: String,
    pub source_key: String,
    pub browse_id: Option<String>,
    pub songs: Vec<Song>,
    pub version: u64,
    pub continuation_token: Option<String>,
    pub continuation_state: ContinuationState,
    #[serde(default)]
    pub diagnostic: Option<ContinuationDiagnostic>,
    #[serde(default)]
    pub seen_continuation_tokens: Vec<String>,
}

/// Immutable page returned by a continuation loader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationPage {
    pub songs: Vec<Song>,
    pub continuation_token: Option<String>,
}

/// Exclusive claim on one continuation token bound to a specific context version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationRequest {
    pub request_id: u64,
    pub context_token: String,
    pub source_key: String,
    pub browse_id: Option<String>,
    pub version: u64,
    pub continuation_token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppendStatus {
    Appended,
    NoNewSongs,
    StaleContext,
    NoActiveContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendResult {
    pub status: AppendStatus,
    pub snapshot: Option<ContextSnapshot>,
    pub added_songs: Vec<Song>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContinuationResultStatus {
    Completed,
    Failed,
    RejectedNoProgress,
    StaleRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationResult {
    pub status: ContinuationResultStatus,
    pub snapshot: Option<ContextSnapshot>,
    pub added_songs: Vec<Song>,
    pub diagnostic: Option<ContinuationDiagnostic>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreError {
    BlankContextToken,
    BlankSourceKey,
    BlankVideoId,
    DuplicateVideoId,
    MissingContinuationToken(ContinuationState),
    BlankSeenToken,
    DuplicateSeenToken,
}

impl fmt::Display for RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlankContextToken => write!(f, "context_token must not be blank"),
            Self::BlankSourceKey => write!(f, "source_key must not be blank"),
            Self::BlankVideoId => write!(f, "restored songs must not contain blank video ids"),
            Self::DuplicateVideoId => {
                write!(f, "restored songs must not contain duplicate video ids")
            }
            Self::MissingContinuationToken(state) => {
                write!(f, "{state:?} snapshots require a continuation token")
            }
            Self::BlankSeenToken => write!(f, "seen continuation tokens must not be blank"),
            Self::DuplicateSeenToken => write!(f, "seen continuation tokens must be unique"),
        }
    }
}

impl std::error::Error for RestoreError {}

type GrowthListener = Arc<dyn Fn(&ContextSnapshot) + Send + Sync>;

struct MutableContext {
    context_token: String,
    source_key: String,
    browse_id: Option<String>,
    songs: Vec<Song>,
    version: u64,
    continuation_token: Option<String>,
    continuation_state: ContinuationState,
    diagnostic: Option<ContinuationDiagnostic>,
    /// Encounter-ordered seen continuation tokens (matches Kotlin's `LinkedHashSet`).
    seen_continuation_tokens: Vec<String>,
    seen_continuation_set: HashSet<String>,
    request: Option<ContinuationRequest>,
}

impl MutableContext {
    fn snapshot(&self) -> ContextSnapshot {
        ContextSnapshot {
            context_token: self.context_token.clone(),
            source_key: self.source_key.clone(),
            browse_id: self.browse_id.clone(),
            songs: self.songs.clone(),
            version: self.version,
            continuation_token: self.continuation_token.clone(),
            continuation_state: self.continuation_state,
            diagnostic: self.diagnostic,
            seen_continuation_tokens: self.seen_continuation_tokens.clone(),
        }
    }

    fn matches(&self, candidate: &ContinuationRequest) -> bool {
        self.continuation_state == ContinuationState::InFlight
            && self.request.as_ref() == Some(candidate)
            && candidate.context_token == self.context_token
            && candidate.version == self.version
            && self.continuation_token.as_deref() == Some(candidate.continuation_token.as_str())
    }
}

#[derive(Default)]
struct RegistryInner {
    active: Option<MutableContext>,
    next_version: u64,
    next_request_id: u64,
    next_listener_id: u64,
    listener_order: Vec<u64>,
    growth_listeners: HashMap<u64, GrowthListener>,
}

impl RegistryInner {
    fn new_version(&mut self) -> u64 {
        self.next_version += 1;
        self.next_version
    }
}

/// Thread-safe, player-free owner of one active browsed context and its continuation lifecycle.
#[derive(Default)]
pub struct ContextRegistry {
    inner: Mutex<RegistryInner>,
}

impl ContextRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_growth_listener<F>(listener: F) -> Self
    where
        F: Fn(&ContextSnapshot) + Send + Sync + 'static,
    {
        let registry = Self::new();
        registry.add_growth_listener(listener);
        registry
    }

    pub fn register(
        &self,
        context_token: impl Into<String>,
        source_key: impl Into<String>,
        browse_id: Option<String>,
        songs: &[Song],
        continuation_token: Option<&str>,
    ) -> ContextSnapshot {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let token = normalize_token(continuation_token);
        let version = inner.new_version();
        let mut seen_list = Vec::new();
        let mut seen_set = HashSet::new();
        if let Some(ref tok) = token {
            seen_list.push(tok.clone());
            seen_set.insert(tok.clone());
        }
        let state = MutableContext {
            context_token: context_token.into(),
            source_key: source_key.into(),
            browse_id,
            songs: deduplicate_songs(songs),
            version,
            continuation_state: if token.is_none() {
                ContinuationState::Exhausted
            } else {
                ContinuationState::Available
            },
            continuation_token: token,
            diagnostic: None,
            seen_continuation_tokens: seen_list,
            seen_continuation_set: seen_set,
            request: None,
        };
        let snapshot = state.snapshot();
        inner.active = Some(state);
        snapshot
    }

    pub fn restore(&self, snapshot: &ContextSnapshot) -> Result<ContextSnapshot, RestoreError> {
        if snapshot.context_token.trim().is_empty() {
            return Err(RestoreError::BlankContextToken);
        }
        if snapshot.source_key.trim().is_empty() {
            return Err(RestoreError::BlankSourceKey);
        }
        let songs = validate_restored_songs(&snapshot.songs)?;
        let token = normalize_token(snapshot.continuation_token.as_deref());
        if matches!(
            snapshot.continuation_state,
            ContinuationState::Available | ContinuationState::InFlight
        ) && token.is_none()
        {
            return Err(RestoreError::MissingContinuationToken(
                snapshot.continuation_state,
            ));
        }

        let mut seen_list = Vec::with_capacity(snapshot.seen_continuation_tokens.len() + 1);
        let mut seen_set = HashSet::with_capacity(snapshot.seen_continuation_tokens.len() + 1);
        for seen_token in &snapshot.seen_continuation_tokens {
            if seen_token.trim().is_empty() {
                return Err(RestoreError::BlankSeenToken);
            }
            if !seen_set.insert(seen_token.clone()) {
                return Err(RestoreError::DuplicateSeenToken);
            }
            seen_list.push(seen_token.clone());
        }
        if let Some(ref tok) = token
            && seen_set.insert(tok.clone())
        {
            seen_list.push(tok.clone());
        }

        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let state_enum = match snapshot.continuation_state {
            ContinuationState::InFlight => ContinuationState::Available,
            other => other,
        };
        let version = inner.new_version();
        let ctx = MutableContext {
            context_token: snapshot.context_token.clone(),
            source_key: snapshot.source_key.clone(),
            browse_id: snapshot.browse_id.clone(),
            songs,
            version,
            continuation_token: token,
            continuation_state: state_enum,
            diagnostic: snapshot.diagnostic,
            seen_continuation_tokens: seen_list,
            seen_continuation_set: seen_set,
            request: None,
        };
        let out = ctx.snapshot();
        inner.active = Some(ctx);
        Ok(out)
    }

    pub fn snapshot(&self) -> Option<ContextSnapshot> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.active.as_ref().map(MutableContext::snapshot)
    }

    pub fn append(
        &self,
        context_token: &str,
        songs: &[Song],
        expected_version: Option<u64>,
    ) -> AppendResult {
        let (result, callback_snapshot) = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let Some(ref state) = inner.active else {
                return AppendResult {
                    status: AppendStatus::NoActiveContext,
                    snapshot: None,
                    added_songs: Vec::new(),
                };
            };
            if state.context_token != context_token
                || expected_version.is_some_and(|v| state.version != v)
            {
                return AppendResult {
                    status: AppendStatus::StaleContext,
                    snapshot: Some(state.snapshot()),
                    added_songs: Vec::new(),
                };
            }

            let added = new_songs(&state.songs, songs);
            if added.is_empty() {
                return AppendResult {
                    status: AppendStatus::NoNewSongs,
                    snapshot: Some(state.snapshot()),
                    added_songs: Vec::new(),
                };
            }

            let new_version = inner.new_version();
            // INFALLIBLE: checked `inner.active` is `Some` above.
            let state = inner.active.as_mut().expect("active checked");
            if state.continuation_state == ContinuationState::InFlight {
                state.request = None;
                state.continuation_state = if state.continuation_token.is_none() {
                    ContinuationState::Exhausted
                } else {
                    ContinuationState::Available
                };
            }
            state.songs.extend(added.iter().cloned());
            state.version = new_version;
            state.diagnostic = None;
            let snap = state.snapshot();
            (
                AppendResult {
                    status: AppendStatus::Appended,
                    snapshot: Some(snap.clone()),
                    added_songs: added,
                },
                Some(snap),
            )
        };

        if let Some(ref snap) = callback_snapshot {
            self.notify_growth(snap);
        }
        result
    }

    pub fn add_growth_listener<F>(&self, listener: F) -> u64
    where
        F: Fn(&ContextSnapshot) + Send + Sync + 'static,
    {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let id = inner.next_listener_id;
        inner.next_listener_id += 1;
        inner.listener_order.push(id);
        inner.growth_listeners.insert(id, Arc::new(listener));
        id
    }

    pub fn remove_growth_listener(&self, id: u64) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.listener_order.retain(|&x| x != id);
        inner.growth_listeners.remove(&id).is_some()
    }

    pub fn claim_continuation(
        &self,
        expected_context_token: Option<&str>,
    ) -> Option<ContinuationRequest> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let req_id = inner.next_request_id;
        let state = inner.active.as_mut()?;
        if expected_context_token.is_some_and(|t| state.context_token != t) {
            return None;
        }
        if state.continuation_state != ContinuationState::Available {
            return None;
        }
        let token = state.continuation_token.clone()?;
        let request = ContinuationRequest {
            request_id: req_id,
            context_token: state.context_token.clone(),
            source_key: state.source_key.clone(),
            browse_id: state.browse_id.clone(),
            version: state.version,
            continuation_token: token,
        };
        state.request = Some(request.clone());
        state.continuation_state = ContinuationState::InFlight;
        inner.next_request_id += 1;
        Some(request)
    }

    pub fn complete(
        &self,
        request: &ContinuationRequest,
        page: ContinuationPage,
    ) -> ContinuationResult {
        let (result, callback_snapshot) = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let is_match = inner
                .active
                .as_ref()
                .is_some_and(|state| state.matches(request));
            if !is_match {
                return ContinuationResult {
                    status: ContinuationResultStatus::StaleRequest,
                    snapshot: inner.active.as_ref().map(MutableContext::snapshot),
                    added_songs: Vec::new(),
                    diagnostic: None,
                    error_message: None,
                };
            }

            let next_token = normalize_token(page.continuation_token.as_deref());
            let (added, diagnostic) = {
                // INFALLIBLE: `is_match` verified `inner.active.is_some()`.
                let state = inner.active.as_ref().expect("active checked");
                let added = new_songs(&state.songs, &page.songs);
                let diag = if added.is_empty() && page.songs.is_empty() {
                    Some(ContinuationDiagnostic::NoProgress)
                } else if added.is_empty() {
                    Some(ContinuationDiagnostic::DuplicatePage)
                } else if next_token
                    .as_deref()
                    .is_some_and(|t| state.seen_continuation_set.contains(t))
                {
                    Some(ContinuationDiagnostic::RepeatedContinuation)
                } else {
                    None
                };
                (added, diag)
            };

            if let Some(diag) = diagnostic {
                let state = inner.active.as_mut().expect("active checked");
                state.request = None;
                state.continuation_state = ContinuationState::Blocked;
                state.diagnostic = Some(diag);
                return ContinuationResult {
                    status: ContinuationResultStatus::RejectedNoProgress,
                    snapshot: Some(state.snapshot()),
                    added_songs: Vec::new(),
                    diagnostic: Some(diag),
                    error_message: None,
                };
            }

            let new_version = inner.new_version();
            let state = inner.active.as_mut().expect("active checked");
            state.request = None;
            state.songs.extend(added.iter().cloned());
            state.continuation_state = match next_token {
                None => ContinuationState::Exhausted,
                Some(ref tok) => {
                    if state.seen_continuation_set.insert(tok.clone()) {
                        state.seen_continuation_tokens.push(tok.clone());
                    }
                    ContinuationState::Available
                }
            };
            state.continuation_token = next_token;
            state.version = new_version;
            state.diagnostic = None;
            let snap = state.snapshot();
            (
                ContinuationResult {
                    status: ContinuationResultStatus::Completed,
                    snapshot: Some(snap.clone()),
                    added_songs: added,
                    diagnostic: None,
                    error_message: None,
                },
                Some(snap),
            )
        };

        if let Some(ref snap) = callback_snapshot {
            self.notify_growth(snap);
        }
        result
    }

    pub fn fail(
        &self,
        request: &ContinuationRequest,
        error_message: Option<String>,
    ) -> ContinuationResult {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let is_match = inner
            .active
            .as_ref()
            .is_some_and(|state| state.matches(request));
        if !is_match {
            return ContinuationResult {
                status: ContinuationResultStatus::StaleRequest,
                snapshot: inner.active.as_ref().map(MutableContext::snapshot),
                added_songs: Vec::new(),
                diagnostic: None,
                error_message: None,
            };
        }

        let state = inner.active.as_mut().expect("active checked");
        state.request = None;
        state.continuation_state = if state.continuation_token.is_none() {
            ContinuationState::Exhausted
        } else {
            ContinuationState::Available
        };
        state.diagnostic = Some(ContinuationDiagnostic::RequestFailed);
        ContinuationResult {
            status: ContinuationResultStatus::Failed,
            snapshot: Some(state.snapshot()),
            added_songs: Vec::new(),
            diagnostic: Some(ContinuationDiagnostic::RequestFailed),
            error_message,
        }
    }

    fn notify_growth(&self, snapshot: &ContextSnapshot) {
        let listeners: Vec<GrowthListener> = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner
                .listener_order
                .iter()
                .filter_map(|id| inner.growth_listeners.get(id).cloned())
                .collect()
        };
        for listener in listeners {
            listener(snapshot);
        }
    }
}

fn normalize_token(token: Option<&str>) -> Option<String> {
    token.filter(|t| !t.trim().is_empty()).map(str::to_owned)
}

fn validate_restored_songs(songs: &[Song]) -> Result<Vec<Song>, RestoreError> {
    let mut ids = HashSet::with_capacity(songs.len());
    for song in songs {
        if song.video_id.trim().is_empty() {
            return Err(RestoreError::BlankVideoId);
        }
        if !ids.insert(context_entry_key(song)) {
            return Err(RestoreError::DuplicateVideoId);
        }
    }
    Ok(songs.to_vec())
}

fn deduplicate_songs(songs: &[Song]) -> Vec<Song> {
    let mut seen = HashSet::with_capacity(songs.len());
    let mut out = Vec::with_capacity(songs.len());
    for song in songs {
        if seen.insert(context_entry_key(song)) {
            out.push(song.clone());
        }
    }
    out
}

fn new_songs(existing: &[Song], candidates: &[Song]) -> Vec<Song> {
    let mut known: HashSet<String> = existing.iter().map(context_entry_key).collect();
    let mut added = Vec::new();
    for candidate in candidates {
        if known.insert(context_entry_key(candidate)) {
            added.push(candidate.clone());
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str) -> Song {
        Song::new(id, id, "Artist", None)
    }

    fn ids(songs: &[Song]) -> Vec<&str> {
        songs.iter().map(|s| s.video_id.as_str()).collect()
    }

    #[test]
    fn register_deduplicates_and_isolates_snapshot() {
        let mut initial = vec![song("a"), song("a"), song("b")];
        let registry = ContextRegistry::new();
        let reg = registry.register(
            "context-a",
            "playlist:one",
            Some("VLone".into()),
            &initial,
            None,
        );
        initial.push(song("c"));

        assert_eq!(vec!["a", "b"], ids(&reg.songs));
        assert_eq!(vec!["a", "b"], ids(&registry.snapshot().unwrap().songs));
    }

    #[test]
    fn continuation_growth_deduplicates_notifies_once_and_advances_token() {
        let growth = Arc::new(Mutex::new(Vec::<ContextSnapshot>::new()));
        let g = Arc::clone(&growth);
        let registry = ContextRegistry::with_growth_listener(move |snap| {
            g.lock().unwrap().push(snap.clone());
        });
        registry.register(
            "context-a",
            "playlist:one",
            Some("VLone".into()),
            &[song("a")],
            Some("page-1"),
        );

        let req = registry.claim_continuation(None).expect("claimed");
        let res = registry.complete(
            &req,
            ContinuationPage {
                songs: vec![song("a"), song("b"), song("b")],
                continuation_token: Some("page-2".into()),
            },
        );

        assert_eq!(ContinuationResultStatus::Completed, res.status);
        assert_eq!(vec!["a", "b"], ids(&res.snapshot.as_ref().unwrap().songs));
        assert_eq!(
            ContinuationState::Available,
            res.snapshot.as_ref().unwrap().continuation_state
        );
        assert_eq!(vec!["b"], ids(&res.added_songs));
        assert_eq!(1, growth.lock().unwrap().len());
        assert_eq!(2, growth.lock().unwrap()[0].version);
        assert_eq!(
            "page-2",
            registry
                .claim_continuation(None)
                .unwrap()
                .continuation_token
        );
    }

    #[test]
    fn stale_duplicate_empty_and_repeated_token_cases() {
        // Stale after new registration
        let registry = ContextRegistry::new();
        registry.register("old", "p:old", None, &[song("old")], Some("old-page"));
        let old_req = registry.claim_continuation(None).unwrap();
        registry.register("new", "p:new", None, &[song("new")], None);
        let stale = registry.complete(
            &old_req,
            ContinuationPage {
                songs: vec![song("nope")],
                continuation_token: Some("next".into()),
            },
        );
        assert_eq!(ContinuationResultStatus::StaleRequest, stale.status);
        assert_eq!(vec!["new"], ids(&registry.snapshot().unwrap().songs));

        // Duplicate page -> Blocked
        registry.register("c1", "p:1", None, &[song("a")], Some("page-1"));
        let req = registry.claim_continuation(None).unwrap();
        let dup = registry.complete(
            &req,
            ContinuationPage {
                songs: vec![song("a")],
                continuation_token: Some("page-2".into()),
            },
        );
        assert_eq!(ContinuationResultStatus::RejectedNoProgress, dup.status);
        assert_eq!(Some(ContinuationDiagnostic::DuplicatePage), dup.diagnostic);
        assert!(registry.claim_continuation(None).is_none());

        // Empty page -> Blocked (NoProgress)
        registry.register("c2", "p:2", None, &[song("a")], Some("page-1"));
        let req2 = registry.claim_continuation(None).unwrap();
        let empty = registry.complete(
            &req2,
            ContinuationPage {
                songs: vec![],
                continuation_token: Some("page-2".into()),
            },
        );
        assert_eq!(Some(ContinuationDiagnostic::NoProgress), empty.diagnostic);
        assert!(registry.claim_continuation(None).is_none());

        // Repeated continuation token -> Blocked even with new song
        registry.register("c3", "p:3", None, &[song("a")], Some("page-1"));
        let req3 = registry.claim_continuation(None).unwrap();
        let rep = registry.complete(
            &req3,
            ContinuationPage {
                songs: vec![song("b")],
                continuation_token: Some("page-1".into()),
            },
        );
        assert_eq!(
            Some(ContinuationDiagnostic::RepeatedContinuation),
            rep.diagnostic
        );
        assert_eq!(vec!["a"], ids(&registry.snapshot().unwrap().songs));
    }

    #[test]
    fn single_in_flight_failure_retry_append_and_restore() {
        let registry = ContextRegistry::new();
        registry.register("c", "p", None, &[song("a")], Some("page-1"));

        let first = registry.claim_continuation(None).unwrap();
        assert!(registry.claim_continuation(None).is_none());
        let failed = registry.fail(&first, Some("network".into()));
        assert_eq!(ContinuationResultStatus::Failed, failed.status);
        assert_eq!(
            Some(ContinuationDiagnostic::RequestFailed),
            failed.diagnostic
        );

        let retry = registry.claim_continuation(None).unwrap();
        assert_ne!(first.request_id, retry.request_id);
        let done = registry.complete(
            &retry,
            ContinuationPage {
                songs: vec![song("b")],
                continuation_token: Some("page-2".into()),
            },
        );
        let persisted = done.snapshot.unwrap();
        assert_eq!(vec!["page-1", "page-2"], persisted.seen_continuation_tokens);

        // Restore remembers seen tokens and blocks repeated token even across process death.
        let restored_reg = ContextRegistry::new();
        let restored = restored_reg.restore(&persisted).expect("valid snapshot");
        assert_ne!(persisted.version, restored.version);
        let next_req = restored_reg.claim_continuation(None).unwrap();
        let blocked = restored_reg.complete(
            &next_req,
            ContinuationPage {
                songs: vec![song("c")],
                continuation_token: Some("page-1".into()),
            },
        );
        assert_eq!(
            Some(ContinuationDiagnostic::RepeatedContinuation),
            blocked.diagnostic
        );

        // Malformed restore rejected cleanly.
        let mut bad = persisted.clone();
        bad.songs.push(song("a"));
        assert_eq!(
            Err(RestoreError::DuplicateVideoId),
            restored_reg.restore(&bad)
        );
        let mut blank = persisted;
        blank.continuation_token = None;
        assert_eq!(
            Err(RestoreError::MissingContinuationToken(
                ContinuationState::Available
            )),
            restored_reg.restore(&blank)
        );
    }
}
