use std::collections::HashSet;
use std::sync::RwLock;

const MAX_REMEMBERED: usize = 20_000;

/// Registry of video IDs known to be the signed-in user's own private uploads,
/// which require an upload-hinted player request.
#[derive(Debug, Default)]
pub struct UploadedTracks {
    ids: RwLock<HashSet<String>>,
}

impl UploadedTracks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark(&self, video_id: &str) {
        if video_id.trim().is_empty() {
            return;
        }
        let mut guard = self.ids.write().unwrap_or_else(|e| e.into_inner());
        if guard.len() >= MAX_REMEMBERED {
            guard.clear();
        }
        guard.insert(video_id.to_owned());
    }

    pub fn contains(&self, video_id: &str) -> bool {
        let guard = self.ids.read().unwrap_or_else(|e| e.into_inner());
        guard.contains(video_id)
    }

    /// Forgets everything — an account switch is a different private library.
    pub fn clear(&self) {
        let mut guard = self.ids.write().unwrap_or_else(|e| e.into_inner());
        guard.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_remembers_forgets_and_ignores_blank() {
        let uploads = UploadedTracks::new();
        assert!(!uploads.contains("abc"));
        uploads.mark("abc");
        assert!(uploads.contains("abc"));
        uploads.clear();
        assert!(!uploads.contains("abc"));
        uploads.mark("");
        assert!(!uploads.contains(""));
    }
}
