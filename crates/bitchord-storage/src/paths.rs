//! XDG Base Directory path resolution for BitChord Linux Desktop.

use std::path::{Path, PathBuf};

use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};

/// Subfolder inside the user's Music directory when `export_downloads` is enabled.
pub const MUSIC_DOWNLOAD_FOLDER: &str = "BitChord";

/// Canonical Linux XDG directory layout for BitChord.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub downloads_private_dir: PathBuf,
    pub downloads_exported_dir: PathBuf,
}

impl AppPaths {
    /// Resolves standard XDG directories (`~/.config/bitchord`, `~/.local/share/bitchord`,
    /// `~/.cache/bitchord`, `~/Music/BitChord`).
    pub fn resolve() -> Self {
        let project = ProjectDirs::from("com.music", "BitChord", "bitchord");
        let (config_dir, data_dir, cache_dir) = match project {
            Some(dirs) => (
                dirs.config_dir().to_path_buf(),
                dirs.data_local_dir().to_path_buf(),
                dirs.cache_dir().to_path_buf(),
            ),
            None => {
                let fallback = std::env::temp_dir().join("bitchord");
                (
                    fallback.join("config"),
                    fallback.join("data"),
                    fallback.join("cache"),
                )
            }
        };

        let downloads_private_dir = data_dir.join("downloads");
        let downloads_exported_dir = UserDirs::new()
            .and_then(|u| u.audio_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| data_dir.join("Music"))
            .join(MUSIC_DOWNLOAD_FOLDER);

        Self {
            config_dir,
            data_dir,
            cache_dir,
            downloads_private_dir,
            downloads_exported_dir,
        }
    }

    /// Creates an isolated directory layout rooted under `root` (for tests or `--data-dir`).
    pub fn in_root(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        let config_dir = root.join("config");
        let data_dir = root.join("data");
        let cache_dir = root.join("cache");
        Self {
            downloads_private_dir: data_dir.join("downloads"),
            downloads_exported_dir: root.join("Music").join(MUSIC_DOWNLOAD_FOLDER),
            config_dir,
            data_dir,
            cache_dir,
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }

    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("library.sqlite3")
    }

    pub fn player_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("player-js")
    }

    pub fn artwork_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("artwork")
    }

    /// Active download target directory given whether `export_downloads` is enabled.
    pub fn active_downloads_dir(&self, export_downloads: bool) -> &Path {
        if export_downloads {
            &self.downloads_exported_dir
        } else {
            &self.downloads_private_dir
        }
    }

    /// Ensures core config, data, cache, and private download directories exist.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        std::fs::create_dir_all(&self.downloads_private_dir)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_root_builds_expected_subpaths() {
        let paths = AppPaths::in_root("/tmp/bitchord-test-root");
        assert_eq!(
            paths.settings_file(),
            PathBuf::from("/tmp/bitchord-test-root/config/settings.json")
        );
        assert_eq!(
            paths.db_file(),
            PathBuf::from("/tmp/bitchord-test-root/data/library.sqlite3")
        );
        assert_eq!(
            paths.active_downloads_dir(false),
            Path::new("/tmp/bitchord-test-root/data/downloads")
        );
        assert_eq!(
            paths.active_downloads_dir(true),
            Path::new("/tmp/bitchord-test-root/Music/BitChord")
        );
    }
}
