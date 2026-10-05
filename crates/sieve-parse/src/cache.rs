//! Per-file extraction memo (P2-35). A warm build reuses one file's nodes and
//! raw edges from the last build, instead of re-reading and re-parsing the
//! file, when its size and modified time still match. See the
//! `build-cards-freshness.md` note section 7.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use sieve_core::Node;

use crate::extract::RawEdge;

/// The version this build writes and expects to read back.
pub const CACHE_VERSION: u32 = 3;

/// The most recent extract cache files Sieve keeps per context directory.
const KEPT_CACHES: usize = 2;

/// One file's cached extraction result: its stat print, its content hash,
/// what extraction produced, or the error extraction raised.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub size: u64,
    #[serde(rename = "mtimeMs")]
    pub mtime_ms: f64,
    pub hash: String,
    pub nodes: Vec<Node>,
    #[serde(rename = "rawEdges")]
    pub raw_edges: Vec<RawEdge>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
}

/// The extraction cache for one build: a version, an extractor stamp, and
/// a per-file entry, keyed by the file's repo-relative id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractCache {
    pub version: u32,
    pub extractor: String,
    pub files: BTreeMap<String, CacheEntry>,
}

/// Builds the extract cache path for one context directory and stamp.
pub fn cache_path(context_dir: &Path, stamp: &str) -> PathBuf {
    context_dir
        .join(".cache")
        .join(format!("extract.{stamp}.json"))
}

/// Reads an extract cache back, when its version and extractor stamp
/// match. Returns an empty cache on a missing file, a parse error, a
/// version mismatch, or a stamp mismatch.
pub fn read_cache(path: &Path, stamp: &str) -> ExtractCache {
    let empty = || ExtractCache {
        version: CACHE_VERSION,
        extractor: stamp.to_string(),
        files: BTreeMap::new(),
    };
    let Ok(body) = fs::read(path) else {
        return empty();
    };
    let Ok(cache) = serde_json::from_slice::<ExtractCache>(&body) else {
        return empty();
    };
    if cache.version != CACHE_VERSION || cache.extractor != stamp {
        return empty();
    }
    cache
}

/// Writes an extract cache atomically, then prunes old cache files.
///
/// This writes compact JSON to a temp file in the same directory, renames
/// it into place, then keeps only the newest [`KEPT_CACHES`] files whose
/// name starts with `extract.` and ends with `.json`.
pub fn write_cache(path: &Path, cache: &ExtractCache) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cache path has no parent"))?;
    fs::create_dir_all(dir)?;

    let body =
        serde_json::to_vec(cache).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

    let tmp_path = dir.join(format!(".extract-tmp-{}", std::process::id()));
    {
        let mut tmp = fs::File::create(&tmp_path)?;
        tmp.write_all(&body)?;
    }
    fs::rename(&tmp_path, path)?;

    prune_caches(dir)
}

fn prune_caches(dir: &Path) -> io::Result<()> {
    let mut entries: Vec<(PathBuf, SystemTime)> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("extract.") && name.ends_with(".json")) {
            continue;
        }
        let metadata = entry.metadata()?;
        let mtime = metadata.modified()?;
        entries.push((entry.path(), mtime));
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (path, _) in entries.into_iter().skip(KEPT_CACHES) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("sieve-extract-cache-{label}-{pid}-{n}-{nanos}"));
            fs::create_dir_all(&path).expect("create temp dir");
            TempDir { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn sample_cache(stamp: &str) -> ExtractCache {
        let mut files = BTreeMap::new();
        files.insert(
            "a.ts".to_string(),
            CacheEntry {
                size: 3,
                mtime_ms: 1.0,
                hash: "deadbeef".to_string(),
                nodes: Vec::new(),
                raw_edges: Vec::new(),
                error: None,
            },
        );
        ExtractCache {
            version: CACHE_VERSION,
            extractor: stamp.to_string(),
            files,
        }
    }

    #[test]
    fn write_then_read_round_trips_when_the_stamp_matches() {
        let dir = TempDir::new("roundtrip");
        let path = cache_path(&dir.path, "stamp1");
        let cache = sample_cache("stamp1");

        write_cache(&path, &cache).expect("write cache");
        let back = read_cache(&path, "stamp1");
        assert_eq!(back, cache);
    }

    #[test]
    fn read_cache_falls_back_to_empty_on_a_missing_file() {
        let dir = TempDir::new("missing");
        let path = cache_path(&dir.path, "stamp1");
        let back = read_cache(&path, "stamp1");
        assert!(back.files.is_empty());
        assert_eq!(back.version, CACHE_VERSION);
        assert_eq!(back.extractor, "stamp1");
    }

    #[test]
    fn read_cache_falls_back_to_empty_on_a_parse_error() {
        let dir = TempDir::new("badjson");
        let path = cache_path(&dir.path, "stamp1");
        fs::create_dir_all(path.parent().unwrap()).expect("create parent");
        fs::write(&path, b"not json").expect("write garbage");
        let back = read_cache(&path, "stamp1");
        assert!(back.files.is_empty());
    }

    #[test]
    fn read_cache_falls_back_to_empty_on_a_version_or_stamp_mismatch() {
        let dir = TempDir::new("mismatch");
        let path = cache_path(&dir.path, "stamp1");
        let cache = sample_cache("stamp1");
        write_cache(&path, &cache).expect("write cache");

        let back = read_cache(&path, "other-stamp");
        assert!(back.files.is_empty());

        let bad_version = ExtractCache {
            version: CACHE_VERSION + 1,
            ..sample_cache("stamp1")
        };
        write_cache(&path, &bad_version).expect("write cache");
        let back = read_cache(&path, "stamp1");
        assert!(back.files.is_empty());
    }

    #[test]
    fn prune_keeps_only_the_two_newest_caches() {
        let dir = TempDir::new("prune");
        let cache_dir = dir.path.join(".cache");
        fs::create_dir_all(&cache_dir).expect("create cache dir");
        let cache = sample_cache("s");
        for label in ["a", "b", "c"] {
            let path = cache_dir.join(format!("extract.{label}.json"));
            write_cache(&path, &cache).expect("write cache");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let remaining: Vec<_> = fs::read_dir(&cache_dir)
            .expect("read cache dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name.starts_with("extract.") && name.ends_with(".json")
            })
            .collect();
        assert_eq!(remaining.len(), KEPT_CACHES);
    }
}
