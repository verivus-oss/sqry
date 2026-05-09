//! Gated archive download + streaming SHA-256 + tar.gz extract.
//!
//! NL03 of the `nl-classifier-load-and-harden` plan. Triggered only
//! when the NL02 resolver returns `None` AND
//! [`crate::TranslatorConfig::allow_model_download`] is `true`. Default
//! posture is OFF — sqry never reaches out to the network unless the
//! operator opts in.
//!
//! ## Trust contract
//!
//! - The trusted **expected manifest** is baked into the binary via
//!   `include_str!("../../models/manifest.json")` (see
//!   [`crate::classifier::baked_manifest`]).
//! - The downloader STREAMS the archive bytes through both
//!   `std::fs::File` (writing to `<archive>.tmp`) AND a [`sha2::Sha256`]
//!   hasher in lock-step. Hashing happens BEFORE extraction so a
//!   tampered payload never touches the on-disk model tree.
//! - On hash mismatch: the partial `.tmp` is deleted and
//!   [`NlError::ManifestSha256Mismatch`] is returned. There is no
//!   `--allow-unverified-model` opt-out for trusted-mode tampering.
//! - On extract error: tempdir cleaned; verified archive may be retained
//!   for retry. [`NlError::DownloadFailed`] is returned.
//!
//! ## Concurrent first-run race
//!
//! Two processes that simultaneously trigger the download path will
//! both write to a per-process `.tmp` file (uniqueness via PID +
//! timestamp), each verify the archive sha256 independently, and then
//! attempt the atomic rename. The loser sees `ErrorKind::AlreadyExists`
//! on either the archive rename or the extract-dir rename — both are
//! tolerated. The "winner" guarantee is purely first-to-rename, and
//! since both processes verified the same trusted hash, the losing
//! payload is byte-identical to the surviving one.
//!
//! ## Out of scope (documented per design §5)
//!
//! - No TLS pinning.
//! - No mirrors.
//! - No retries beyond `ureq`'s defaults.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use tar::Archive;

use crate::classifier::manifest::Manifest;
use crate::error::{NlError, NlResult};

/// Pluggable byte-stream source.
///
/// Production calls use [`UreqDownloader`]. Tests use
/// [`FileDownloader`], which resolves `file://` URLs against the local
/// filesystem so the test suite never touches the network.
pub trait Downloader {
    /// Stream bytes from `url` into `sink`, returning the total bytes
    /// written on success.
    ///
    /// Implementations MUST write the body in streaming chunks (the
    /// caller pipes those bytes through both a SHA-256 hasher and a
    /// disk file — buffering the whole archive in memory would defeat
    /// that design).
    ///
    /// # Errors
    ///
    /// Returns [`NlError::DownloadFailed`] on transport, HTTP-status,
    /// or sink-IO error.
    fn fetch(&self, url: &str, sink: &mut dyn Write) -> NlResult<u64>;
}

/// Connect-phase timeout for the model-archive download. A stalled TLS
/// handshake against an unreachable mirror MUST NOT hang the daemon /
/// MCP / CLI process indefinitely.
const UREQ_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Read-phase timeout for the model-archive download. Bounds the total
/// time we will wait on a single body chunk; a stuck server cannot pin
/// the process forever.
const UREQ_READ_TIMEOUT: Duration = Duration::from_secs(300);

/// Shared `ureq::Agent` configured with explicit connect/read timeouts.
/// Built lazily on first use so test runs that never hit
/// [`UreqDownloader`] pay no setup cost.
fn ureq_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_connect(Some(UREQ_CONNECT_TIMEOUT))
            .timeout_recv_response(Some(UREQ_READ_TIMEOUT))
            .timeout_recv_body(Some(UREQ_READ_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

/// Production [`Downloader`] backed by `ureq`.
///
/// Uses a shared agent configured with explicit connect (30s) and read
/// (300s) timeouts so a stalled TLS handshake or slow server cannot hang
/// the daemon / MCP / CLI process. No TLS pinning, no retries beyond
/// ureq's defaults. Non-2xx HTTP responses are mapped to
/// [`NlError::DownloadFailed`].
pub struct UreqDownloader;

impl Downloader for UreqDownloader {
    fn fetch(&self, url: &str, sink: &mut dyn Write) -> NlResult<u64> {
        let response = ureq_agent()
            .get(url)
            .call()
            .map_err(|e| NlError::DownloadFailed(format!("ureq GET {url} failed: {e}")))?;

        if response.status().as_u16() < 200 || response.status().as_u16() >= 300 {
            return Err(NlError::DownloadFailed(format!(
                "HTTP {} from {url}",
                response.status().as_u16()
            )));
        }

        let mut reader = response.into_body().into_reader();
        let written = io::copy(&mut reader, sink)
            .map_err(|e| NlError::DownloadFailed(format!("body copy from {url} failed: {e}")))?;
        Ok(written)
    }
}

/// Test-only [`Downloader`] that resolves `file://` URLs against the
/// local filesystem. Used by the integration tests in
/// `sqry-nl/tests/download_gated.rs`.
pub struct FileDownloader;

impl Downloader for FileDownloader {
    fn fetch(&self, url: &str, sink: &mut dyn Write) -> NlResult<u64> {
        let path = url.strip_prefix("file://").ok_or_else(|| {
            NlError::DownloadFailed(format!("FileDownloader requires file:// URL, got {url:?}"))
        })?;
        let mut file =
            File::open(path).map_err(|e| NlError::DownloadFailed(format!("open {path}: {e}")))?;
        let written = io::copy(&mut file, sink)
            .map_err(|e| NlError::DownloadFailed(format!("read {path}: {e}")))?;
        Ok(written)
    }
}

/// Ensure the trusted model tree is present at `cache_dir`, downloading
/// it on demand when permitted.
///
/// This is the public, URL-driven entrypoint. It picks
/// [`UreqDownloader`] under the hood — the test suite uses
/// [`ensure_model_in_cache_with`] to inject [`FileDownloader`] instead.
///
/// # Behaviour
///
/// - If `cache_dir/manifest.json` already exists, returns
///   `Ok(cache_dir.to_path_buf())` immediately. This second-chance
///   check covers the race in which another writer populated the cache
///   between the resolver miss and this call.
/// - If `allow_download` is `false`, returns
///   [`NlError::DownloadDisabled`].
/// - Otherwise creates `cache_dir`, streams the archive through
///   SHA-256 + disk in lock-step, atomic-renames the verified archive
///   into place, extracts into a sibling staging tempdir, then
///   atomic-renames the extracted contents into `cache_dir`.
///
/// # Errors
///
/// - [`NlError::DownloadDisabled`] — `allow_download == false` and the
///   cache is empty.
/// - [`NlError::ManifestSha256Mismatch`] — streamed bytes hashed to a
///   value other than `expected_manifest.sha256`.
/// - [`NlError::DownloadFailed`] — transport, HTTP, or extraction IO
///   error. The staging tempdir (if any) is cleaned up before return.
/// - [`NlError::Io`] — local filesystem error not otherwise mappable.
pub fn ensure_model_in_cache(
    cache_dir: &Path,
    expected_manifest: &Manifest,
    allow_download: bool,
) -> NlResult<PathBuf> {
    ensure_model_in_cache_with(
        cache_dir,
        expected_manifest,
        allow_download,
        &UreqDownloader,
    )
}

/// Variant of [`ensure_model_in_cache`] that accepts an explicit
/// [`Downloader`]. Public so the integration tests can inject
/// [`FileDownloader`] without going through the network.
///
/// # Errors
///
/// See [`ensure_model_in_cache`].
pub fn ensure_model_in_cache_with(
    cache_dir: &Path,
    expected_manifest: &Manifest,
    allow_download: bool,
    downloader: &dyn Downloader,
) -> NlResult<PathBuf> {
    // Second-chance hit check (resolver may have raced with another
    // process between its lookup and this call).
    if cache_dir.join("manifest.json").exists() {
        return Ok(cache_dir.to_path_buf());
    }

    if !allow_download {
        return Err(NlError::DownloadDisabled);
    }

    fs::create_dir_all(cache_dir)?;

    // ----- 1. Stream + hash + write to <archive>.tmp -----------------
    let archive_path = cache_dir.join(&expected_manifest.archive);
    let tmp_path = unique_tmp_path(&archive_path);

    let actual_hash =
        stream_to_file_with_hash(downloader, &expected_manifest.download_url, &tmp_path)
            .inspect_err(|_| {
                // Best-effort cleanup; ignore secondary errors.
                let _ = fs::remove_file(&tmp_path);
            })?;

    if !sha256_eq(&actual_hash, &expected_manifest.sha256) {
        let _ = fs::remove_file(&tmp_path);
        return Err(NlError::ManifestSha256Mismatch {
            file: expected_manifest.archive.clone(),
            expected: expected_manifest.sha256.clone(),
            actual: actual_hash,
        });
    }

    // ----- 2. Atomic-rename verified archive into place --------------
    if let Err(e) = fs::rename(&tmp_path, &archive_path) {
        match e.kind() {
            io::ErrorKind::AlreadyExists => {
                // Another process won the race AND it had to verify the
                // same trusted hash to get here — payload bytes are
                // equivalent. Drop our copy and continue.
                let _ = fs::remove_file(&tmp_path);
            }
            _ => {
                let _ = fs::remove_file(&tmp_path);
                return Err(NlError::DownloadFailed(format!(
                    "rename {} -> {}: {e}",
                    tmp_path.display(),
                    archive_path.display()
                )));
            }
        }
    }

    // ----- 3. Extract into staging tempdir ---------------------------
    let staging = unique_extract_dir(cache_dir);
    if let Err(e) = extract_targz_into(&archive_path, &staging) {
        // Recursive cleanup; tolerate failure (best effort).
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }

    // ----- 4. Promote extracted tree into cache_dir ------------------
    if let Err(e) = promote_extracted(&staging, cache_dir) {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }

    Ok(cache_dir.to_path_buf())
}

/// Stream bytes from `downloader` into `path`, computing SHA-256 over
/// each chunk before it is written. Returns the lowercase hex-encoded
/// digest on success.
fn stream_to_file_with_hash(
    downloader: &dyn Downloader,
    url: &str,
    path: &Path,
) -> NlResult<String> {
    let file = File::create(path)
        .map_err(|e| NlError::DownloadFailed(format!("create {}: {e}", path.display())))?;
    let mut sink = HashingWriter {
        inner: file,
        hasher: Sha256::new(),
    };
    downloader.fetch(url, &mut sink)?;
    sink.inner
        .sync_all()
        .map_err(|e| NlError::DownloadFailed(format!("sync_all {}: {e}", path.display())))?;
    Ok(hex_lower(&sink.hasher.finalize()))
}

/// `Write` adapter that tees its input through a SHA-256 hasher.
struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        if n > 0 {
            self.hasher.update(&buf[..n]);
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Constant-time-ish equality on lowercased hex strings. The hashes are
/// non-secret but we still avoid early-exit on mismatched bytes.
fn sha256_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= if x.eq_ignore_ascii_case(y) { 0 } else { 1 };
    }
    diff == 0
}

/// Lowercase hex encoding.
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Build a unique `.tmp` sibling for an archive path. Includes PID +
/// nanos since epoch to avoid collisions across concurrent first-run
/// downloaders.
fn unique_tmp_path(archive_path: &Path) -> PathBuf {
    let mut buf = archive_path.as_os_str().to_owned();
    buf.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        unique_suffix_nanos()
    ));
    PathBuf::from(buf)
}

/// Build a unique staging directory for archive extraction.
fn unique_extract_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join(format!(
        ".extract.tmp.{}.{}",
        std::process::id(),
        unique_suffix_nanos()
    ))
}

fn unique_suffix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// Decompress + untar `archive_path` into `into` (which must not yet
/// exist). On any IO failure during extraction, the partially-extracted
/// tree under `into` is removed by the caller before propagating.
fn extract_targz_into(archive_path: &Path, into: &Path) -> NlResult<()> {
    fs::create_dir_all(into).map_err(|e| {
        NlError::DownloadFailed(format!("create staging dir {}: {e}", into.display()))
    })?;
    let file = File::open(archive_path)
        .map_err(|e| NlError::DownloadFailed(format!("open {}: {e}", archive_path.display())))?;
    let gz = GzDecoder::new(file);
    let mut archive = Archive::new(gz);
    archive.unpack(into).map_err(|e| {
        NlError::DownloadFailed(format!(
            "untar {} -> {}: {e}",
            archive_path.display(),
            into.display()
        ))
    })
}

/// Move every entry from `staging` into `cache_dir`. The archive may
/// have one of two layouts:
///
/// 1. `staging/manifest.json` etc. directly (flat layout).
/// 2. `staging/<single-top-dir>/manifest.json` etc. (nested layout —
///    common when the archive was built with a `tar c sqry-models-vX/`
///    invocation).
///
/// Both are accepted. The function detects the nested layout by looking
/// for a single top-level directory containing `manifest.json` and
/// flattens it transparently.
fn promote_extracted(staging: &Path, cache_dir: &Path) -> NlResult<()> {
    let source = pick_extracted_root(staging)?;

    for entry in fs::read_dir(&source)
        .map_err(|e| NlError::DownloadFailed(format!("read_dir {}: {e}", source.display())))?
    {
        let entry = entry.map_err(|e| {
            NlError::DownloadFailed(format!("dir entry under {}: {e}", source.display()))
        })?;
        let from = entry.path();
        let to = cache_dir.join(entry.file_name());

        match fs::rename(&from, &to) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // Another writer populated the same name. Trusted-mode
                // verification means both copies are byte-identical;
                // drop ours.
                let _ = if from.is_dir() {
                    fs::remove_dir_all(&from)
                } else {
                    fs::remove_file(&from)
                };
            }
            Err(e) => {
                return Err(NlError::DownloadFailed(format!(
                    "promote {} -> {}: {e}",
                    from.display(),
                    to.display()
                )));
            }
        }
    }

    // Best-effort: drop the now-empty staging dir.
    let _ = fs::remove_dir_all(staging);
    Ok(())
}

/// Determine which path inside `staging` holds the model tree.
fn pick_extracted_root(staging: &Path) -> NlResult<PathBuf> {
    if staging.join("manifest.json").is_file() {
        return Ok(staging.to_path_buf());
    }

    let entries: Vec<_> = fs::read_dir(staging)
        .map_err(|e| NlError::DownloadFailed(format!("read_dir {}: {e}", staging.display())))?
        .collect::<Result<_, _>>()
        .map_err(|e| {
            NlError::DownloadFailed(format!("dir entry under {}: {e}", staging.display()))
        })?;

    if entries.len() == 1 {
        let candidate = entries[0].path();
        if candidate.is_dir() && candidate.join("manifest.json").is_file() {
            return Ok(candidate);
        }
    }

    Err(NlError::DownloadFailed(format!(
        "extracted archive at {} does not contain a manifest.json",
        staging.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_eq_compares_lowercase_hex() {
        assert!(sha256_eq("abc123", "ABC123"));
        assert!(sha256_eq("abc123", "abc123"));
        assert!(!sha256_eq("abc123", "abc124"));
        assert!(!sha256_eq("abc", "abcd"));
    }

    #[test]
    fn hex_lower_roundtrips_known_values() {
        assert_eq!(hex_lower(&[0x00, 0xff, 0xab]), "00ffab");
        assert_eq!(hex_lower(&[]), "");
    }
}
