//! Resolves and fetches a pack *reference* (`file://`, bare path, `http(s)://`,
//! `oci://`, `repo://`, `store://`) into a local cache directory, returning a
//! path to a `.gtpack` artifact (or, for a local directory reference, the
//! directory itself).
//!
//! Scheme classification is split into a pure function ([`map_scheme`]) so it
//! can be unit-tested without mutating process environment variables.

use operax_core::{OperaxError, Result, sha256_digest};
use std::fs;
use std::path::{Path, PathBuf};

/// Env var carrying the OCI registry base that `repo://` references resolve
/// against.
const REPO_REGISTRY_BASE_ENV: &str = "GREENTIC_REPO_REGISTRY_BASE";

/// Env var carrying the OCI registry base that `store://` references resolve
/// against.
const STORE_REGISTRY_BASE_ENV: &str = "GREENTIC_STORE_REGISTRY_BASE";

/// Where a pack reference resolves to, before any network/disk I/O happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A single local `.gtpack` (or other) file to be hashed and cached.
    LocalFile(PathBuf),
    /// A local directory (e.g. an already-unpacked handoff dir). Used as-is.
    LocalDir(PathBuf),
    /// An `http://` or `https://` URL to download.
    Http(String),
    /// An OCI image reference (`registry/repo:tag` or `registry/repo@digest`).
    Oci(String),
}

/// Pure scheme classifier for a pack reference string.
///
/// - `oci://<rest>` -> `Oci(rest)`.
/// - `repo://<rest>` -> `Oci("{repo_base}/{rest}")`; errors if `repo_base` is
///   `None`.
/// - `store://<rest>` -> `Oci("{store_base}/{rest}")`; errors if `store_base`
///   is `None`.
/// - `http://` / `https://` -> `Http(reference)`.
/// - `file://<path>` or a bare path -> `LocalDir` if the path is an existing
///   directory, otherwise `LocalFile`.
pub fn map_scheme(
    reference: &str,
    repo_base: Option<&str>,
    store_base: Option<&str>,
) -> Result<Resolved> {
    if let Some(rest) = reference.strip_prefix("oci://") {
        return Ok(Resolved::Oci(rest.to_string()));
    }
    if let Some(rest) = reference.strip_prefix("repo://") {
        let base = repo_base.ok_or_else(|| {
            OperaxError::new(
                "missing_repo_registry_base",
                format!(
                    "repo:// references require {REPO_REGISTRY_BASE_ENV} to be set (reference: {reference})"
                ),
            )
        })?;
        return Ok(Resolved::Oci(format!("{base}/{rest}")));
    }
    if let Some(rest) = reference.strip_prefix("store://") {
        let base = store_base.ok_or_else(|| {
            OperaxError::new(
                "missing_store_registry_base",
                format!(
                    "store:// references require {STORE_REGISTRY_BASE_ENV} to be set (reference: {reference})"
                ),
            )
        })?;
        return Ok(Resolved::Oci(format!("{base}/{rest}")));
    }
    if reference.starts_with("http://") || reference.starts_with("https://") {
        return Ok(Resolved::Http(reference.to_string()));
    }
    let path = match reference.strip_prefix("file://") {
        Some(rest) => Path::new(rest),
        None => Path::new(reference),
    };
    if path.is_dir() {
        Ok(Resolved::LocalDir(path.to_path_buf()))
    } else {
        Ok(Resolved::LocalFile(path.to_path_buf()))
    }
}

/// Fetches a pack reference into `cache_dir`, returning the local path to the
/// artifact.
///
/// A `LocalDir` reference is returned as-is (a directory cannot be hashed
/// into a single cache file). All other kinds are read/downloaded, hashed
/// with SHA-256, and written to `cache_dir/<hex-digest>.gtpack` (the write is
/// skipped when that path already exists, making repeat fetches idempotent).
pub fn fetch_pack_ref(reference: &str, cache_dir: &Path) -> Result<PathBuf> {
    let repo_base = std::env::var(REPO_REGISTRY_BASE_ENV).ok();
    let store_base = std::env::var(STORE_REGISTRY_BASE_ENV).ok();
    let resolved = map_scheme(reference, repo_base.as_deref(), store_base.as_deref())?;

    fs::create_dir_all(cache_dir).map_err(|err| {
        OperaxError::new(
            "pack_fetch_failed",
            format!(
                "failed to create pack cache dir {}: {err}",
                cache_dir.display()
            ),
        )
    })?;

    match resolved {
        Resolved::LocalDir(path) => Ok(path),
        Resolved::LocalFile(path) => {
            let bytes = fs::read(&path).map_err(|err| {
                OperaxError::new(
                    "pack_fetch_failed",
                    format!("failed to read pack reference {}: {err}", path.display()),
                )
            })?;
            write_to_cache(cache_dir, &bytes)
        }
        Resolved::Http(url) => {
            let bytes = fetch_http_bytes(&url)?;
            write_to_cache(cache_dir, &bytes)
        }
        Resolved::Oci(image) => pull_oci_blob(&image, cache_dir),
    }
}

/// Downloads `url` fully into memory via a blocking `ureq` GET.
fn fetch_http_bytes(url: &str) -> Result<Vec<u8>> {
    let mut response = ureq::get(url).call().map_err(|err| {
        OperaxError::new("pack_fetch_failed", format!("http GET {url} failed: {err}"))
    })?;
    response
        .body_mut()
        .with_config()
        .limit(512 * 1024 * 1024)
        .read_to_vec()
        .map_err(|err| {
            OperaxError::new(
                "pack_fetch_failed",
                format!("failed to read http response body from {url}: {err}"),
            )
        })
}

/// Writes `bytes` to `cache_dir/<hex-sha256>.gtpack`, skipping the write if a
/// file with that content hash is already cached.
fn write_to_cache(cache_dir: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let digest = sha256_digest(bytes);
    let hex = digest.strip_prefix("sha256:").unwrap_or(&digest);
    let dest = cache_dir.join(format!("{hex}.gtpack"));
    if !dest.is_file() {
        fs::write(&dest, bytes).map_err(|err| {
            OperaxError::new(
                "pack_fetch_failed",
                format!("failed to write cached pack {}: {err}", dest.display()),
            )
        })?;
    }
    Ok(dest)
}

/// Pulls an OCI image reference and caches the resulting pack.
///
/// Placeholder for Task 1: OCI fetch support lands in Task 2, which replaces
/// this body with a real `oci-distribution` pull. Returning an honest error
/// here (rather than a fake success) keeps `fetch_pack_ref` truthful about
/// what it currently supports.
fn pull_oci_blob(_image_ref: &str, _cache_dir: &Path) -> Result<PathBuf> {
    Err(OperaxError::new(
        "oci_fetch_unavailable",
        "OCI fetch lands in Task 2",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_repo_and_store_schemes_from_env() {
        assert_eq!(
            map_scheme(
                "repo://acme/widget:1.0",
                Some("registry.example.com/repo"),
                None
            )
            .expect("repo scheme maps"),
            Resolved::Oci("registry.example.com/repo/acme/widget:1.0".to_string())
        );
        assert_eq!(
            map_scheme(
                "store://acme/widget:1.0",
                None,
                Some("registry.example.com/store")
            )
            .expect("store scheme maps"),
            Resolved::Oci("registry.example.com/store/acme/widget:1.0".to_string())
        );

        let repo_err = map_scheme("repo://acme/widget:1.0", None, None).unwrap_err();
        assert_eq!(repo_err.code, "missing_repo_registry_base");
        let store_err = map_scheme("store://acme/widget:1.0", None, None).unwrap_err();
        assert_eq!(store_err.code, "missing_store_registry_base");

        assert_eq!(
            map_scheme("oci://ghcr.io/acme/widget:1.0", None, None).expect("oci scheme maps"),
            Resolved::Oci("ghcr.io/acme/widget:1.0".to_string())
        );
        assert_eq!(
            map_scheme("https://example.com/pack.gtpack", None, None).expect("http scheme maps"),
            Resolved::Http("https://example.com/pack.gtpack".to_string())
        );
        assert_eq!(
            map_scheme("http://example.com/pack.gtpack", None, None).expect("http scheme maps"),
            Resolved::Http("http://example.com/pack.gtpack".to_string())
        );
    }

    #[test]
    fn fetches_local_file_into_cache() {
        let tmp = std::env::temp_dir().join(format!("obr-src-{}.gtpack", std::process::id()));
        fs::write(&tmp, b"PK\x03\x04 fake pack bytes").expect("write source file");
        let cache = std::env::temp_dir().join(format!("obr-cache-{}", std::process::id()));

        let got = fetch_pack_ref(&format!("file://{}", tmp.display()), &cache).expect("fetch file");
        assert!(got.starts_with(&cache));
        assert_eq!(
            fs::read(&got).expect("read cached file"),
            b"PK\x03\x04 fake pack bytes"
        );

        // Idempotent: second call returns the same cached path.
        let again =
            fetch_pack_ref(&format!("file://{}", tmp.display()), &cache).expect("fetch again");
        assert_eq!(got, again);
    }

    #[test]
    fn passes_through_a_local_directory() {
        let dir = std::env::temp_dir().join(format!("obr-dir-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create source dir");
        let cache = std::env::temp_dir().join(format!("obr-dirc-{}", std::process::id()));

        let got =
            fetch_pack_ref(dir.to_str().expect("utf8 path"), &cache).expect("dir passthrough");
        assert_eq!(got, dir);
    }

    #[test]
    fn fetches_over_http() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let addr = listener.local_addr().expect("listener addr");
        let body: &[u8] = b"PK\x03\x04 http fake pack bytes";

        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(body);
                let _ = stream.flush();
            }
        });

        let url = format!("http://{addr}/pack.gtpack");
        let cache = std::env::temp_dir().join(format!("obr-http-cache-{}", std::process::id()));

        let got = fetch_pack_ref(&url, &cache).expect("fetch http");
        server.join().expect("http test server thread");

        assert!(got.starts_with(&cache));
        assert_eq!(fs::read(&got).expect("read cached http body"), body);
    }
}
