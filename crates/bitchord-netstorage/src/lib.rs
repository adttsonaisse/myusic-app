#![forbid(unsafe_code)]

//! Pure-Rust WebDAV and SMB2/3 network music library clients, folder cover-art
//! pairing, and seekable chunk-cached byte-range streaming readers.
//!
//! Mirrors upstream `data/webdav/`, `data/smb/`, and `data/remote/` semantics so
//! tracks from NAS shares and Nextcloud/WebDAV servers appear seamlessly in the
//! same library, queue, and audio engine as local and YouTube Music tracks.

pub mod remote_song;
pub mod smb;
pub mod webdav;

pub use remote_song::{
    AUDIO_EXTENSIONS, IMAGE_EXTENSIONS, MAX_COVER_BYTES, build_remote_song, is_audio_file,
    is_image_file, pick_artwork, split_artist_title,
};
pub use smb::{
    ChunkedReaderCore, RangeReadBackend, SMB_BROWSE_ID, SMB_DEFAULT_PORT, SmbClient, SmbConfig,
    SmbEntry, SmbError, SmbListing, SmbRangeReader,
};
pub use webdav::{
    WEBDAV_BROWSE_ID, WebDavClient, WebDavConfig, WebDavEntry, WebDavError, WebDavListing,
    WebDavPutResult,
};
