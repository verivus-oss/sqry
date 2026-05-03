//! NL03 acceptance tests for the gated downloader.
//!
//! These tests build hand-rolled `tar.gz` archives in tempdirs, then
//! point a [`FileDownloader`] at them via `file://` URLs. No network
//! access ever occurs; CI is hermetic.
//!
//! Coverage maps directly onto the DAG `[units.NL03]` acceptance list:
//!
//! | DAG name                                       | Test fn                                                 |
//! |------------------------------------------------|---------------------------------------------------------|
//! | download_disabled_returns_disabled_error       | `download_disabled_returns_disabled_error`              |
//! | download_enabled_writes_archive_and_verifies   | `download_enabled_writes_archive_and_verifies`          |
//! | archive_sha256_mismatch_is_fatal               | `archive_sha256_mismatch_is_fatal`                      |
//! | manifest_parse_failed_surfaces                 | `manifest_parse_failed_surfaces`                        |
//! | partial_extract_cleaned_on_error               | `partial_extract_cleaned_on_error`                      |

#![cfg(feature = "classifier")]

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use sqry_nl::NlError;
use sqry_nl::classifier::download::{
    FileDownloader, ensure_model_in_cache, ensure_model_in_cache_with,
};
use sqry_nl::classifier::manifest::Manifest;

const ARCHIVE_NAME: &str = "sqry-models-test.tar.gz";

/// Build a tar.gz containing `manifest.json` + the supplied files at the
/// archive root (flat layout). Returns the on-disk path of the archive
/// AND its SHA-256 hex digest (computed over the on-the-wire bytes).
fn build_targz(dir: &Path, name: &str, members: &[(&str, &[u8])]) -> (PathBuf, String) {
    let archive_path = dir.join(name);
    let file = File::create(&archive_path).expect("create archive");
    let gz = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(gz);
    for (entry_name, body) in members {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, entry_name, *body)
            .expect("append");
    }
    builder
        .into_inner()
        .expect("finalize tar")
        .finish()
        .expect("finish gz");

    let bytes = fs::read(&archive_path).expect("read archive back");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hex_lower(&hasher.finalize());
    (archive_path, digest)
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn manifest_for(archive_name: &str, sha256: &str, download_url: &str) -> Manifest {
    let json = format!(
        r#"{{
            "model_version": "0.0.0-test",
            "release_tag":   "test",
            "archive":       {archive:?},
            "sha256":        {sha:?},
            "download_url":  {url:?},
            "files": {{
                "manifest.json":  "ignored-by-nl03",
                "intent_classifier.onnx": "ignored-by-nl03"
            }}
        }}"#,
        archive = archive_name,
        sha = sha256,
        url = download_url,
    );
    Manifest::parse(&json).expect("parse synthesized manifest")
}

fn file_url_for(path: &Path) -> String {
    format!("file://{}", path.display())
}

#[test]
fn download_disabled_returns_disabled_error() {
    let tmp = TempDir::new().unwrap();
    let cache = tmp.path().join("cache");
    // No on-disk model yet, no permission to download → DownloadDisabled.
    let manifest = manifest_for(ARCHIVE_NAME, "0".repeat(64).as_str(), "file:///dev/null");

    let err = ensure_model_in_cache(&cache, &manifest, false).unwrap_err();
    assert!(
        matches!(err, NlError::DownloadDisabled),
        "expected DownloadDisabled, got {err:?}"
    );
    // No partial cache directory should have been created.
    assert!(
        !cache.join("manifest.json").exists(),
        "downloader populated cache despite allow_download=false"
    );
}

#[test]
fn download_enabled_writes_archive_and_verifies() {
    let tmp = TempDir::new().unwrap();
    let staging = tmp.path().join("source");
    fs::create_dir_all(&staging).unwrap();

    let manifest_body = br#"{
        "model_version": "0.0.0-test",
        "release_tag":   "test",
        "archive":       "sqry-models-test.tar.gz",
        "sha256":        "00",
        "download_url":  "ignored",
        "files":         {}
    }"#;
    let onnx_body: &[u8] = b"fake onnx model bytes";
    let (archive_path, archive_sha) = build_targz(
        &staging,
        ARCHIVE_NAME,
        &[
            ("manifest.json", manifest_body),
            ("intent_classifier.onnx", onnx_body),
        ],
    );

    let manifest = manifest_for(ARCHIVE_NAME, &archive_sha, &file_url_for(&archive_path));
    let cache = tmp.path().join("cache");

    let result = ensure_model_in_cache_with(&cache, &manifest, true, &FileDownloader)
        .expect("download succeeds");
    assert_eq!(result, cache);

    assert!(cache.join("manifest.json").exists(), "manifest extracted");
    assert!(
        cache.join("intent_classifier.onnx").exists(),
        "model file extracted"
    );
    // The verified archive should also be left in cache_dir alongside
    // the extracted contents (callers may want to re-verify or
    // re-extract without re-downloading).
    assert!(
        cache.join(ARCHIVE_NAME).exists(),
        "verified archive retained"
    );

    // No staging tempdir should leak.
    let staging_leaks: Vec<_> = fs::read_dir(&cache)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".extract.tmp"))
        .collect();
    assert!(
        staging_leaks.is_empty(),
        "staging directory leaked: {staging_leaks:?}"
    );
}

#[test]
fn archive_sha256_mismatch_is_fatal() {
    let tmp = TempDir::new().unwrap();
    let staging = tmp.path().join("source");
    fs::create_dir_all(&staging).unwrap();

    let (archive_path, real_sha) = build_targz(&staging, ARCHIVE_NAME, &[("manifest.json", b"{}")]);

    // Lie about the sha — flip the last hex char.
    let mut wrong = real_sha.clone();
    let last = wrong.pop().unwrap();
    wrong.push(if last == 'a' { 'b' } else { 'a' });

    let manifest = manifest_for(ARCHIVE_NAME, &wrong, &file_url_for(&archive_path));
    let cache = tmp.path().join("cache");

    let err = ensure_model_in_cache_with(&cache, &manifest, true, &FileDownloader).unwrap_err();
    match err {
        NlError::ManifestSha256Mismatch {
            file,
            expected,
            actual,
        } => {
            assert_eq!(file, ARCHIVE_NAME);
            assert_eq!(expected, wrong);
            assert_eq!(actual, real_sha);
        }
        other => panic!("expected ManifestSha256Mismatch, got {other:?}"),
    }

    // Half-installed cache MUST NOT exist.
    assert!(
        !cache.join("manifest.json").exists(),
        "extracted manifest leaked through hash-mismatch path"
    );
    let leftover_tmp: Vec<_> = fs::read_dir(&cache)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(
        leftover_tmp.is_empty(),
        ".tmp file leaked through hash-mismatch path: {leftover_tmp:?}"
    );
}

#[test]
fn manifest_parse_failed_surfaces() {
    let err = Manifest::parse("{not valid json at all").unwrap_err();
    assert!(
        matches!(err, NlError::ManifestParseFailed(_)),
        "expected ManifestParseFailed, got {err:?}"
    );

    // The Display impl should at least mention the wrapping context so
    // operator-facing surfaces (CLI / MCP / LSP) print something
    // actionable.
    let rendered = err.to_string();
    assert!(
        rendered.contains("Model manifest parse failed"),
        "Display lost context: {rendered:?}"
    );
}

#[test]
fn partial_extract_cleaned_on_error() {
    let tmp = TempDir::new().unwrap();
    let staging = tmp.path().join("source");
    fs::create_dir_all(&staging).unwrap();

    // Write a "tar.gz" whose gzip header decodes successfully but whose
    // tar payload is garbage — this exercises the late-extract failure
    // path (downloader writes archive, sha matches, extraction blows up).
    let archive_path = staging.join(ARCHIVE_NAME);
    {
        let file = File::create(&archive_path).unwrap();
        let mut gz = GzEncoder::new(file, Compression::default());
        // 1 KiB of zero bytes is a perfectly valid gzip stream but
        // decodes to a tar parser error (no entries, no end-of-archive
        // markers in the right places).
        gz.write_all(&[0u8; 1024]).unwrap();
        gz.finish().unwrap();
    }

    // Append something extra after the gzip stream so tar::Archive
    // definitely fails partway through extract.
    {
        use std::fs::OpenOptions;
        let mut f = OpenOptions::new().append(true).open(&archive_path).unwrap();
        f.write_all(b"trailing garbage that breaks tar parsing")
            .unwrap();
    }

    let bytes = fs::read(&archive_path).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let real_sha = hex_lower(&hasher.finalize());

    let manifest = manifest_for(ARCHIVE_NAME, &real_sha, &file_url_for(&archive_path));
    let cache = tmp.path().join("cache");

    let err = ensure_model_in_cache_with(&cache, &manifest, true, &FileDownloader).unwrap_err();
    assert!(
        matches!(err, NlError::DownloadFailed(_)),
        "expected DownloadFailed, got {err:?}"
    );

    // Critical: no manifest.json (success marker) in cache_dir.
    assert!(
        !cache.join("manifest.json").exists(),
        "manifest.json materialised despite extraction failure"
    );

    // Critical: no .extract.tmp.* directory left behind.
    let leftover: Vec<_> = fs::read_dir(&cache)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".extract.tmp"))
        .collect();
    assert!(
        leftover.is_empty(),
        "staging .extract.tmp/ leaked through extraction-failure path: {leftover:?}"
    );
}

/// Smoke test: the `BTreeMap<String, String>` deterministic-ordering
/// claim in the manifest module docs holds for the synthesized manifests
/// used above.
#[test]
fn manifest_files_iteration_is_sorted() {
    let manifest = manifest_for("x.tar.gz", "00", "file:///dev/null");
    let keys: Vec<_> = manifest.files.keys().cloned().collect();
    let mut expected = keys.clone();
    expected.sort();
    assert_eq!(keys, expected, "BTreeMap iteration must be lexicographic");

    // Belt-and-braces: the type alias of the field is BTreeMap, not HashMap.
    let _: BTreeMap<String, String> = manifest.files;
}
