use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use rquickjs::{Context, Runtime};

use crate::error::StreamError;

const DEFAULT_MEMORY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_STACK_LIMIT_BYTES: usize = 1024 * 1024;
const DEFAULT_EVAL_TIMEOUT: Duration = Duration::from_secs(5);
const KEPT_PREPROCESSED_PLAYERS: usize = 3;

/// Sandboxed QuickJS-NG (`rquickjs 0.14.0`) evaluator for YouTube `sig` and `n`
/// ciphers and `yt-dlp/ejs`-compatible solver payloads.
///
/// Executing YouTube transforms inside QuickJS guarantees exact ECMAScript
/// UTF-16 `.split("")` / `.slice()` / `.charCodeAt()` semantics (avoiding
/// Rust UTF-8 byte vs UTF-16 code-unit mismatches) while enforcing a strict
/// memory limit (64 MiB) and execution deadline.
#[derive(Debug, Clone)]
pub struct CipherSandbox {
    memory_limit_bytes: usize,
    timeout: Duration,
    cache_dir: Option<PathBuf>,
}

impl Default for CipherSandbox {
    fn default() -> Self {
        Self::new(None)
    }
}

impl CipherSandbox {
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        Self {
            memory_limit_bytes: DEFAULT_MEMORY_LIMIT_BYTES,
            timeout: DEFAULT_EVAL_TIMEOUT,
            cache_dir,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_memory_limit(mut self, bytes: usize) -> Self {
        self.memory_limit_bytes = bytes;
        self
    }

    /// Evaluates a JavaScript expression inside an isolated QuickJS-NG runtime
    /// with memory and wall-clock execution limits, returning its string result.
    pub fn eval_to_string(&self, script: &str) -> Result<String, StreamError> {
        let rt = Runtime::new()
            .map_err(|e| StreamError::Transient(format!("quickjs runtime init failed: {e}")))?;
        rt.set_memory_limit(self.memory_limit_bytes);
        rt.set_max_stack_size(DEFAULT_STACK_LIMIT_BYTES);

        let deadline = Instant::now() + self.timeout;
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));

        let ctx = Context::full(&rt)
            .map_err(|e| StreamError::Transient(format!("quickjs context init failed: {e}")))?;

        ctx.with(|ctx| {
            ctx.eval::<String, _>(script)
                .map_err(|e| StreamError::Transient(format!("quickjs eval failed: {e}")))
        })
    }

    /// Runs `eval_to_string` on Tokio's blocking pool so heavy JS parsing never
    /// stalls async network tasks.
    pub async fn eval_to_string_async(&self, script: String) -> Result<String, StreamError> {
        let sandbox = self.clone();
        tokio::task::spawn_blocking(move || sandbox.eval_to_string(&script))
            .await
            .map_err(|e| StreamError::Transient(format!("quickjs worker join error: {e}")))?
    }

    /// Invokes a JS transform function `fn_source` (e.g. `(s) => ...`) with a
    /// single string input, passing inputs/outputs via JSON encoding so the
    /// Rust↔QuickJS boundary remains pure UTF-8.
    pub fn run_string_transform(
        &self,
        fn_source: &str,
        input: &str,
    ) -> Result<String, StreamError> {
        let arg_json = serde_json::to_string(input)
            .map_err(|e| StreamError::Transient(format!("encode cipher arg: {e}")))?;
        let script = format!("String(({fn_source})({arg_json}))");
        self.eval_to_string(&script)
    }

    /// Loads a previously cached preprocessed player script by `player_hash`.
    pub fn load_preprocessed_player(&self, player_hash: &str) -> Option<String> {
        let dir = self.cache_dir.as_ref()?;
        let safe = sanitize_hash(player_hash)?;
        fs::read_to_string(dir.join(format!("{safe}.js"))).ok()
    }

    /// Atomically persists a preprocessed player script and prunes older files,
    /// retaining at most [`KEPT_PREPROCESSED_PLAYERS`] (3) newest players.
    pub fn save_preprocessed_player(&self, player_hash: &str, script: &str) -> std::io::Result<()> {
        let Some(ref dir) = self.cache_dir else {
            return Ok(());
        };
        let Some(safe) = sanitize_hash(player_hash) else {
            return Ok(());
        };
        fs::create_dir_all(dir)?;
        let dest = dir.join(format!("{safe}.js"));
        let tmp = dir.join(format!("{safe}.tmp"));
        fs::write(&tmp, script)?;
        fs::rename(&tmp, &dest)?;
        prune_old_players(dir, KEPT_PREPROCESSED_PLAYERS);
        Ok(())
    }
}

fn sanitize_hash(raw: &str) -> Option<String> {
    let clean: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if clean.is_empty() { None } else { Some(clean) }
}

fn prune_old_players(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, SystemTime)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("js") {
                return None;
            }
            let modified = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some((path, modified))
        })
        .collect();
    files.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (path, _) in files.into_iter().skip(keep) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executes_youtube_style_sig_transform_with_utf16_semantics() {
        let sandbox = CipherSandbox::default();
        // Classic YouTube sig splice/reverse/swap sequence + UTF-16 length check.
        let out = sandbox
            .run_string_transform(
                r#"(s) => {
                    const a = s.split("");
                    a.reverse();
                    const tmp = a[0]; a[0] = a[2]; a[2] = tmp;
                    return a.slice(1).join("");
                }"#,
                "abcdef",
            )
            .expect("eval succeeded");
        // "abcdef" -> reverse "fedcba" -> swap(0,2) "defcba" -> slice(1) "efcba"
        assert_eq!(out, "efcba");

        // Verify native ECMAScript UTF-16 code-unit length for astral emoji.
        let len = sandbox
            .eval_to_string(r#"String("🎵a".length)"#)
            .expect("utf16 length");
        assert_eq!(len, "3");
    }

    #[test]
    fn interrupts_infinite_loop_within_timeout() {
        let sandbox = CipherSandbox::default().with_timeout(Duration::from_millis(80));
        let err = sandbox
            .eval_to_string("while (true) {}")
            .expect_err("infinite loop must be interrupted");
        assert!(matches!(err, StreamError::Transient(_)));
    }

    #[test]
    fn preprocessed_player_cache_keeps_at_most_three_newest() {
        let dir = std::env::temp_dir().join(format!("bitchord-qjs-test-{}", uuid::Uuid::new_v4()));
        let sandbox = CipherSandbox::new(Some(dir.clone()));
        for i in 0..5 {
            sandbox
                .save_preprocessed_player(&format!("player_{i}"), &format!("/* p{i} */"))
                .expect("save");
            std::thread::sleep(Duration::from_millis(10));
        }
        let remaining: Vec<_> = fs::read_dir(&dir)
            .expect("read_dir")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(remaining.len(), 3);
        assert_eq!(
            sandbox.load_preprocessed_player("player_4").as_deref(),
            Some("/* p4 */")
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
