//! UI-independent proxy and derived-preview cache infrastructure.
//!
//! The module intentionally has no decoder, renderer, or GPUI dependency.  A
//! caller supplies a [`ProxyGenerator`] which returns owned bytes, and this
//! module handles bounded publication, cache identity, cancellation, and
//! latest-request-wins scheduling.
//!
//! Cache limits are explicit:
//!
//! * [`CacheLimits::max_bytes`] bounds published bytes on disk;
//! * [`CacheLimits::max_entries`] bounds the number of published artifacts;
//! * [`CacheLimits::max_entry_bytes`] rejects an individual oversized artifact;
//! * [`ProxyWorker`] has one pending request and a caller-selected bounded
//!   completion channel.
//!
//! Temporary files are written beside their destination and committed with a
//! hard link.  A hard link creates the destination atomically with
//! `create_new`/no-replace semantics, so a competing publisher can never
//! overwrite an existing cache artifact.  Generators must cooperate with the
//! [`CancellationToken`]; no thread can safely force-stop arbitrary Rust code.

use crate::project::{AssetId, FrameRate, Time};
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};

/// A monotonic request generation, scoped to the caller's project/source
/// revision.  Generation zero is valid and is the initial generation.
pub type Generation = u64;

/// Prefix used for final cache artifacts.  Other files in the cache root are
/// not counted or removed by [`ProxyCache`].
pub const CACHE_FILE_PREFIX: &str = "slicer-proxy-";
const CACHE_FILE_SUFFIX: &str = ".bin";
const TEMP_FILE_MARKER: &str = ".tmp-";
const TEMP_CHUNK_BYTES: usize = 64 * 1024;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

/// The representation/container family requested for a proxy artifact.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyFormat {
    /// Raw interleaved RGBA bytes, useful for a decoded preview artifact.
    #[default]
    Rgba8,
    /// Planar 4:2:0 bytes, useful for a decoder/interchange cache.
    Yuv420p,
    /// Encoded proxy media.  The actual encoder remains the generator's job.
    H264,
}

/// The output characteristics that distinguish one proxy rendition from
/// another.  Transform revisions and source times live in [`ProxyKey`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxySpec {
    pub width: u32,
    pub height: u32,
    pub format: ProxyFormat,
    /// Quality is deliberately an opaque 0..=100 setting.  Its interpretation
    /// belongs to the generator/encoder selected by the caller.
    pub quality: u8,
    pub frame_rate: Option<FrameRate>,
}

impl ProxySpec {
    /// Construct a valid default raw proxy specification for the dimensions.
    pub const fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            format: ProxyFormat::Rgba8,
            quality: 0,
            frame_rate: None,
        }
    }

    pub const fn with_format(width: u32, height: u32, format: ProxyFormat, quality: u8) -> Self {
        Self {
            width,
            height,
            format,
            quality,
            frame_rate: None,
        }
    }

    pub const fn with_quality(mut self, quality: u8) -> Self {
        self.quality = quality;
        self
    }

    pub const fn with_frame_rate(mut self, frame_rate: Option<FrameRate>) -> Self {
        self.frame_rate = frame_rate;
        self
    }

    pub fn validate(self) -> Result<(), ProxyError> {
        if self.width == 0 || self.height == 0 {
            return Err(ProxyError::InvalidSpec(
                "proxy dimensions must be positive".to_owned(),
            ));
        }
        if self.quality > 100 {
            return Err(ProxyError::InvalidSpec(
                "proxy quality must be in the range 0..=100".to_owned(),
            ));
        }
        if let Some(frame_rate) = self.frame_rate
            && (frame_rate.numerator == 0 || frame_rate.denominator == 0)
        {
            return Err(ProxyError::InvalidSpec(
                "proxy frame rate must be positive".to_owned(),
            ));
        }
        Ok(())
    }
}

impl Default for ProxySpec {
    fn default() -> Self {
        Self::new(320, 180)
    }
}

/// Full identity of a derived proxy/cache artifact.
///
/// The generation is intentionally absent.  A generated artifact can be
/// reused by a newer seek generation when its source/revision/time/transform
/// identity is unchanged; generation belongs to [`ProxyRequest`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyKey {
    pub asset_id: AssetId,
    pub source_revision: u64,
    pub decode_time: Time,
    pub transform_revision: u64,
    pub spec: ProxySpec,
}

impl ProxyKey {
    pub const fn new(
        asset_id: AssetId,
        source_revision: u64,
        decode_time: Time,
        transform_revision: u64,
        spec: ProxySpec,
    ) -> Self {
        Self {
            asset_id,
            source_revision,
            decode_time,
            transform_revision,
            spec,
        }
    }

    pub fn validate(self) -> Result<(), ProxyError> {
        if self.asset_id.is_zero() {
            return Err(ProxyError::InvalidKey(
                "proxy asset identity must be non-zero".to_owned(),
            ));
        }
        if self.decode_time < Time::ZERO {
            return Err(ProxyError::InvalidKey(
                "proxy decode time must be non-negative".to_owned(),
            ));
        }
        self.spec.validate().map_err(|error| match error {
            ProxyError::InvalidSpec(message) => ProxyError::InvalidKey(message),
            other => other,
        })
    }

    /// Return a canonical, fixed-width encoding of every key field.
    ///
    /// This is deliberately not Rust's process-randomized `DefaultHasher`.
    /// Canonical `Time` already reduces equivalent fractions, and the fixed
    /// endian encoding makes the resulting cache path stable across runs and
    /// platforms.
    pub fn canonical_bytes(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(96);
        bytes.extend_from_slice(b"slicer-proxy-key-v1\0");
        bytes.extend_from_slice(&self.asset_id.value().to_be_bytes());
        bytes.extend_from_slice(&self.source_revision.to_be_bytes());
        bytes.extend_from_slice(&self.decode_time.numerator.to_be_bytes());
        bytes.extend_from_slice(&self.decode_time.denominator.to_be_bytes());
        bytes.extend_from_slice(&self.transform_revision.to_be_bytes());
        bytes.extend_from_slice(&self.spec.width.to_be_bytes());
        bytes.extend_from_slice(&self.spec.height.to_be_bytes());
        bytes.push(match self.spec.format {
            ProxyFormat::Rgba8 => 0,
            ProxyFormat::Yuv420p => 1,
            ProxyFormat::H264 => 2,
        });
        bytes.push(self.spec.quality);
        match self.spec.frame_rate {
            Some(frame_rate) => {
                bytes.push(1);
                bytes.extend_from_slice(&frame_rate.numerator.to_be_bytes());
                bytes.extend_from_slice(&frame_rate.denominator.to_be_bytes());
            }
            None => bytes.push(0),
        }
        bytes
    }

    /// A collision-free filesystem-safe token derived from the canonical key.
    pub fn stable_token(self) -> String {
        hex_encode(&self.canonical_bytes())
    }

    pub fn cache_path(self, root: impl AsRef<Path>) -> PathBuf {
        deterministic_cache_path(root, &self)
    }
}

/// Return the deterministic final path for a key below `root`.
pub fn deterministic_cache_path(root: impl AsRef<Path>, key: &ProxyKey) -> PathBuf {
    root.as_ref().join(format!(
        "{CACHE_FILE_PREFIX}{}{CACHE_FILE_SUFFIX}",
        key.stable_token()
    ))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

/// Errors returned by cache validation, bounded publication, and workers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyError {
    InvalidSpec(String),
    InvalidKey(String),
    InvalidLimits(String),
    Io {
        operation: String,
        path: PathBuf,
        message: String,
    },
    Cancelled,
    StaleGeneration,
    WorkerStopped,
    CachePoisoned,
    Generator(String),
}

impl ProxyError {
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl fmt::Display for ProxyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSpec(message) => {
                write!(formatter, "invalid proxy specification: {message}")
            }
            Self::InvalidKey(message) => write!(formatter, "invalid proxy key: {message}"),
            Self::InvalidLimits(message) => write!(formatter, "invalid proxy limits: {message}"),
            Self::Io {
                operation,
                path,
                message,
            } => write!(formatter, "{operation} {}: {message}", path.display()),
            Self::Cancelled => formatter.write_str("proxy request was cancelled"),
            Self::StaleGeneration => formatter.write_str("proxy request has a stale generation"),
            Self::WorkerStopped => formatter.write_str("proxy worker is stopped"),
            Self::CachePoisoned => formatter.write_str("proxy cache lock was poisoned"),
            Self::Generator(message) => write!(formatter, "proxy generator failed: {message}"),
        }
    }
}

impl Error for ProxyError {}

fn io_error(operation: &str, path: &Path, error: io::Error) -> ProxyError {
    ProxyError::Io {
        operation: operation.to_owned(),
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

/// Independent limits for the persistent proxy cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheLimits {
    /// Maximum aggregate size of published cache artifacts.
    pub max_bytes: u64,
    /// Maximum number of published cache artifacts.
    pub max_entries: usize,
    /// Maximum size of one published cache artifact.
    pub max_entry_bytes: u64,
}

impl CacheLimits {
    pub const fn new(max_bytes: u64, max_entries: usize, max_entry_bytes: u64) -> Self {
        Self {
            max_bytes,
            max_entries,
            max_entry_bytes,
        }
    }

    /// Zero for any limit disables publication at that bound.  This makes a
    /// cache easy to disable without a separate boolean configuration.
    pub const fn disabled() -> Self {
        Self::new(0, 0, 0)
    }

    pub const fn max_disk_bytes(self) -> u64 {
        self.max_bytes
    }

    pub fn validate(self) -> Result<(), ProxyError> {
        // Zero is meaningful (disabled); all fields are unsigned and thus
        // cannot be negative.  Keep this method so callers can validate a
        // config at an API boundary and future limits have one home.
        Ok(())
    }
}

/// A cache artifact known to be a regular file at `path`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyArtifact {
    pub key: ProxyKey,
    pub path: PathBuf,
    pub size: u64,
}

/// Result of a publication attempt.  `RejectedOversize` never creates a
/// destination; `AlreadyPresent` never changes the destination's bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishOutcome {
    pub status: PublishStatus,
    pub path: PathBuf,
    /// Actual destination size for a published/already-present artifact, or
    /// the attempted size for a rejected artifact.
    pub bytes: u64,
    pub artifact: Option<ProxyArtifact>,
    pub evicted_entries: usize,
    pub evicted_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishStatus {
    Published,
    AlreadyPresent,
    RejectedOversize,
}

/// LRU accounting information for a [`ProxyCache`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProxyCacheStats {
    pub entries: usize,
    pub bytes: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

/// Report returned by explicit or publication-triggered LRU eviction.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EvictionReport {
    pub entries: usize,
    pub bytes: u64,
    /// Keys are included for artifacts published during this cache instance;
    /// artifacts discovered from a previous process may have no in-memory key.
    pub keys: Vec<ProxyKey>,
}

struct CacheEntry {
    key: Option<ProxyKey>,
    size: u64,
    last_used: u64,
}

/// A disk-backed, revision-safe LRU cache for proxy/derived artifacts.
pub struct ProxyCache {
    root: PathBuf,
    limits: CacheLimits,
    bytes: u64,
    tick: u64,
    entries: HashMap<PathBuf, CacheEntry>,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl ProxyCache {
    pub fn new(root: impl AsRef<Path>, limits: CacheLimits) -> Result<Self, ProxyError> {
        limits.validate()?;
        let root = root.as_ref().to_path_buf();
        if root.as_os_str().is_empty() {
            return Err(ProxyError::InvalidLimits(
                "proxy cache root must not be empty".to_owned(),
            ));
        }
        fs::create_dir_all(&root)
            .map_err(|error| io_error("create cache directory", &root, error))?;
        let metadata = fs::symlink_metadata(&root)
            .map_err(|error| io_error("inspect cache directory", &root, error))?;
        if !metadata.is_dir() {
            return Err(ProxyError::InvalidLimits(format!(
                "proxy cache root is not a directory: {}",
                root.display()
            )));
        }

        let mut cache = Self {
            root,
            limits,
            bytes: 0,
            tick: 0,
            entries: HashMap::new(),
            hits: 0,
            misses: 0,
            evictions: 0,
        };
        cache.cleanup_temporary_files()?;
        cache.reconcile()?;
        cache.evict_to_budget()?;
        Ok(cache)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn limits(&self) -> CacheLimits {
        self.limits
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn stats(&self) -> ProxyCacheStats {
        ProxyCacheStats {
            entries: self.len(),
            bytes: self.bytes,
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
        }
    }

    pub fn path_for(&self, key: &ProxyKey) -> PathBuf {
        deterministic_cache_path(&self.root, key)
    }

    /// Re-scan final artifacts and update byte accounting.  This is useful
    /// after another process wins a destination race or after a cache root is
    /// reused by a new process.
    pub fn reconcile(&mut self) -> Result<(), ProxyError> {
        let mut discovered = Vec::new();
        let directory = fs::read_dir(&self.root)
            .map_err(|error| io_error("scan cache directory", &self.root, error))?;
        for entry in directory {
            let entry =
                entry.map_err(|error| io_error("read cache directory entry", &self.root, error))?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !is_final_cache_name(name) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| io_error("inspect cache artifact", &path, error))?;
            if metadata.file_type().is_file() {
                discovered.push((path, metadata.len()));
            }
        }
        discovered.sort_by(|left, right| left.0.cmp(&right.0));

        let mut previous = std::mem::take(&mut self.entries);
        let mut entries = HashMap::with_capacity(discovered.len());
        let mut bytes = 0_u64;
        for (path, size) in discovered {
            let entry = if let Some(existing) = previous.remove(&path) {
                CacheEntry { size, ..existing }
            } else {
                self.tick = self.tick.saturating_add(1);
                CacheEntry {
                    key: None,
                    size,
                    last_used: self.tick,
                }
            };
            bytes = bytes.saturating_add(size);
            entries.insert(path, entry);
        }
        self.entries = entries;
        self.bytes = bytes;
        Ok(())
    }

    /// Remove abandoned same-directory temporary files left by an interrupted
    /// publisher.  Only names generated by this module are considered.
    pub fn cleanup_temporary_files(&self) -> Result<usize, ProxyError> {
        let mut removed = 0;
        let directory = fs::read_dir(&self.root)
            .map_err(|error| io_error("scan temporary cache files", &self.root, error))?;
        for entry in directory {
            let entry =
                entry.map_err(|error| io_error("read cache directory entry", &self.root, error))?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !is_temporary_cache_name(name) {
                continue;
            }
            match fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error("remove temporary cache file", &path, error)),
            }
        }
        Ok(removed)
    }

    /// Return a hit and mark it most-recently-used.  Both `key` and `&key`
    /// forms are accepted to mirror the existing frame-cache ergonomics.
    pub fn get<K>(&mut self, key: K) -> Result<Option<ProxyArtifact>, ProxyError>
    where
        K: Borrow<ProxyKey>,
    {
        self.lookup(key.borrow())
    }

    pub fn lookup(&mut self, key: &ProxyKey) -> Result<Option<ProxyArtifact>, ProxyError> {
        let key = *key;
        key.validate()?;
        let path = self.path_for(&key);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.remove_entry(&path);
                self.misses = self.misses.saturating_add(1);
                return Ok(None);
            }
            Err(error) => return Err(io_error("inspect proxy cache hit", &path, error)),
        };
        if !metadata.file_type().is_file() {
            self.remove_entry(&path);
            self.misses = self.misses.saturating_add(1);
            return Ok(None);
        }
        self.register_path(path.clone(), key, metadata.len());
        self.touch(&path);
        self.hits = self.hits.saturating_add(1);
        Ok(Some(ProxyArtifact {
            key,
            path,
            size: metadata.len(),
        }))
    }

    pub fn read_bytes<K>(&mut self, key: K) -> Result<Option<Vec<u8>>, ProxyError>
    where
        K: Borrow<ProxyKey>,
    {
        let Some(artifact) = self.get(key)? else {
            return Ok(None);
        };
        fs::read(&artifact.path)
            .map(Some)
            .map_err(|error| io_error("read proxy cache artifact", &artifact.path, error))
    }

    pub fn contains<K>(&self, key: K) -> bool
    where
        K: Borrow<ProxyKey>,
    {
        let key = *key.borrow();
        if key.validate().is_err() {
            return false;
        }
        let path = self.path_for(&key);
        fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_file())
            .unwrap_or(false)
    }

    pub fn remove<K>(&mut self, key: K) -> Result<bool, ProxyError>
    where
        K: Borrow<ProxyKey>,
    {
        let key = *key.borrow();
        key.validate()?;
        let path = self.path_for(&key);
        let existed = match fs::symlink_metadata(&path) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(io_error("inspect proxy cache artifact", &path, error)),
        };
        if existed {
            fs::remove_file(&path)
                .map_err(|error| io_error("remove proxy cache artifact", &path, error))?;
        }
        self.remove_entry(&path);
        Ok(existed)
    }

    /// Remove every final artifact currently managed by this cache.
    pub fn clear(&mut self) -> Result<usize, ProxyError> {
        let paths = self.entries.keys().cloned().collect::<Vec<_>>();
        let mut removed = 0;
        for path in paths {
            match fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error("clear proxy cache artifact", &path, error)),
            }
            self.remove_entry(&path);
        }
        Ok(removed)
    }

    /// Evict least-recently-used entries until the configured budget holds.
    pub fn evict_to_budget(&mut self) -> Result<EvictionReport, ProxyError> {
        let mut report = self.evict_oversized_entries()?;
        let budget = self.evict_until(0, 0)?;
        report.entries += budget.entries;
        report.bytes = report.bytes.saturating_add(budget.bytes);
        report.keys.extend(budget.keys);
        Ok(report)
    }

    /// Publish bytes through a cancellable atomic no-overwrite commit.
    pub fn publish_bytes(
        &mut self,
        key: ProxyKey,
        bytes: &[u8],
    ) -> Result<PublishOutcome, ProxyError> {
        let token = CancellationToken::new();
        self.publish_bytes_with_cancel(key, bytes, &token)
    }

    pub fn publish_bytes_with_cancel(
        &mut self,
        key: ProxyKey,
        bytes: &[u8],
        cancel: &CancellationToken,
    ) -> Result<PublishOutcome, ProxyError> {
        key.validate()?;
        let path = self.path_for(&key);
        let attempted_bytes = u64::try_from(bytes.len()).map_err(|_| {
            ProxyError::InvalidLimits("artifact byte length does not fit in u64".to_owned())
        })?;

        self.reconcile()?;
        if let Some(outcome) = self.occupied_outcome(&path, key)? {
            return Ok(outcome);
        }

        if self.limits.max_bytes == 0
            || self.limits.max_entries == 0
            || self.limits.max_entry_bytes == 0
            || attempted_bytes > self.limits.max_bytes
            || attempted_bytes > self.limits.max_entry_bytes
        {
            return Ok(PublishOutcome {
                status: PublishStatus::RejectedOversize,
                path,
                bytes: attempted_bytes,
                artifact: None,
                evicted_entries: 0,
                evicted_bytes: 0,
            });
        }
        cancel.check()?;

        let temporary = write_temporary(&self.root, &path, bytes, cancel)?;

        let result = (|| {
            cancel.check()?;
            self.reconcile()?;
            if let Some(outcome) = self.occupied_outcome(&path, key)? {
                return Ok(outcome);
            }
            let evicted = self.evict_until(attempted_bytes, 1)?;
            cancel.check()?;

            match fs::hard_link(&temporary, &path) {
                Ok(()) => {
                    self.register_path(path.clone(), key, attempted_bytes);
                    Ok(PublishOutcome {
                        status: PublishStatus::Published,
                        path,
                        bytes: attempted_bytes,
                        artifact: Some(ProxyArtifact {
                            key,
                            path: self.path_for(&key),
                            size: attempted_bytes,
                        }),
                        evicted_entries: evicted.entries,
                        evicted_bytes: evicted.bytes,
                    })
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    self.reconcile()?;
                    self.occupied_outcome(&path, key)?.ok_or_else(|| {
                        io_error(
                            "observe raced proxy destination",
                            &path,
                            io::Error::new(
                                io::ErrorKind::NotFound,
                                "destination disappeared after no-replace commit lost",
                            ),
                        )
                    })
                }
                Err(error) => Err(io_error("atomically publish proxy artifact", &path, error)),
            }
        })();

        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) if result.is_ok() => {
                return Err(io_error("remove proxy temporary file", &temporary, error));
            }
            Err(_) => {}
        }
        result
    }

    fn occupied_outcome(
        &mut self,
        path: &Path,
        key: ProxyKey,
    ) -> Result<Option<PublishOutcome>, ProxyError> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error("inspect proxy destination", path, error)),
        };
        let size = metadata.len();
        let artifact = if metadata.file_type().is_file() {
            self.register_path(path.to_path_buf(), key, size);
            Some(ProxyArtifact {
                key,
                path: path.to_path_buf(),
                size,
            })
        } else {
            self.remove_entry(path);
            None
        };
        Ok(Some(PublishOutcome {
            status: PublishStatus::AlreadyPresent,
            path: path.to_path_buf(),
            bytes: artifact.as_ref().map_or(size, |artifact| artifact.size),
            artifact,
            evicted_entries: 0,
            evicted_bytes: 0,
        }))
    }

    fn register_path(&mut self, path: PathBuf, key: ProxyKey, size: u64) {
        if let Some(entry) = self.entries.get_mut(&path) {
            self.bytes = self.bytes.saturating_sub(entry.size).saturating_add(size);
            entry.key = Some(key);
            entry.size = size;
            return;
        }
        self.tick = self.tick.saturating_add(1);
        self.bytes = self.bytes.saturating_add(size);
        self.entries.insert(
            path,
            CacheEntry {
                key: Some(key),
                size,
                last_used: self.tick,
            },
        );
    }

    fn touch(&mut self, path: &Path) {
        self.tick = self.tick.saturating_add(1);
        if let Some(entry) = self.entries.get_mut(path) {
            entry.last_used = self.tick;
        }
    }

    fn remove_entry(&mut self, path: &Path) -> Option<CacheEntry> {
        let entry = self.entries.remove(path)?;
        self.bytes = self.bytes.saturating_sub(entry.size);
        Some(entry)
    }

    fn evict_until(
        &mut self,
        additional_bytes: u64,
        additional_entries: usize,
    ) -> Result<EvictionReport, ProxyError> {
        let mut report = EvictionReport::default();
        while self.entries.len().saturating_add(additional_entries) > self.limits.max_entries
            || self.bytes.saturating_add(additional_bytes) > self.limits.max_bytes
        {
            let Some(path) = self
                .entries
                .iter()
                .min_by(|(left_path, left), (right_path, right)| {
                    (left.last_used, *left_path).cmp(&(right.last_used, *right_path))
                })
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            let Some(entry) = self.entries.get(&path) else {
                continue;
            };
            let key = entry.key;
            let size = entry.size;
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error("evict proxy cache artifact", &path, error)),
            }
            self.remove_entry(&path);
            self.evictions = self.evictions.saturating_add(1);
            report.entries += 1;
            report.bytes = report.bytes.saturating_add(size);
            if let Some(key) = key {
                report.keys.push(key);
            }
        }
        Ok(report)
    }

    fn evict_oversized_entries(&mut self) -> Result<EvictionReport, ProxyError> {
        let oversized = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                self.limits.max_entry_bytes == 0 || entry.size > self.limits.max_entry_bytes
            })
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        let mut report = EvictionReport::default();
        for path in oversized {
            let Some(entry) = self.entries.get(&path) else {
                continue;
            };
            let key = entry.key;
            let size = entry.size;
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error("evict oversized proxy artifact", &path, error)),
            }
            self.remove_entry(&path);
            self.evictions = self.evictions.saturating_add(1);
            report.entries += 1;
            report.bytes = report.bytes.saturating_add(size);
            if let Some(key) = key {
                report.keys.push(key);
            }
        }
        Ok(report)
    }
}

fn is_final_cache_name(name: &str) -> bool {
    name.starts_with(CACHE_FILE_PREFIX)
        && name.ends_with(CACHE_FILE_SUFFIX)
        && !name.contains(TEMP_FILE_MARKER)
}

fn is_temporary_cache_name(name: &str) -> bool {
    name.starts_with('.') && name.contains(CACHE_FILE_PREFIX) && name.contains(TEMP_FILE_MARKER)
}

fn write_temporary(
    root: &Path,
    destination: &Path,
    bytes: &[u8],
    cancel: &CancellationToken,
) -> Result<PathBuf, ProxyError> {
    let file_name = destination
        .file_name()
        .ok_or_else(|| ProxyError::InvalidLimits("proxy destination has no filename".to_owned()))?
        .to_string_lossy();
    let process_id = std::process::id();
    let mut file = None;
    let mut temporary_path = None;
    for _ in 0..128 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let candidate = root.join(format!(".{file_name}{TEMP_FILE_MARKER}{process_id}-{id}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(created) => {
                file = Some(created);
                temporary_path = Some(candidate);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error("create proxy temporary file", &candidate, error)),
        }
    }
    let Some(mut file) = file else {
        return Err(ProxyError::Io {
            operation: "reserve proxy temporary file".to_owned(),
            path: root.to_path_buf(),
            message: "unable to allocate a unique temporary filename".to_owned(),
        });
    };
    let temporary_path = temporary_path.expect("file and temporary path are created together");
    let result = (|| {
        for chunk in bytes.chunks(TEMP_CHUNK_BYTES) {
            cancel.check()?;
            file.write_all(chunk)
                .map_err(|error| io_error("write proxy temporary file", &temporary_path, error))?;
        }
        cancel.check()?;
        file.flush()
            .map_err(|error| io_error("flush proxy temporary file", &temporary_path, error))?;
        file.sync_all()
            .map_err(|error| io_error("sync proxy temporary file", &temporary_path, error))?;
        Ok(())
    })();
    drop(file);
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    Ok(temporary_path)
}

/// A cooperative cancellation flag shared with a generator and the worker.
#[derive(Clone, Debug)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn check(&self) -> Result<(), ProxyError> {
        if self.is_cancelled() {
            Err(ProxyError::Cancelled)
        } else {
            Ok(())
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

/// A generation-aware request.  Generation does not enter [`ProxyKey`], so a
/// successful artifact can serve multiple seeks without duplicate files.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProxyRequest {
    pub key: ProxyKey,
    pub generation: Generation,
}

impl ProxyRequest {
    pub const fn new(key: ProxyKey, generation: Generation) -> Self {
        Self { key, generation }
    }

    pub fn validate(self) -> Result<(), ProxyError> {
        self.key.validate()
    }
}

/// Handle returned by [`ProxyWorker::submit`].  Submitting another request
/// automatically cancels the currently active request; this handle allows a
/// caller to cancel one request without advancing the whole generation.
#[derive(Clone, Debug)]
pub struct ProxySubmission {
    pub request: ProxyRequest,
    pub sequence: u64,
    token: CancellationToken,
}

impl ProxySubmission {
    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

/// Owned result delivered by a proxy worker.  The completion channel is
/// bounded and delivery is best-effort (`try_send`), so a slow consumer never
/// makes a low-priority worker unbounded or blocking.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyCompletion {
    pub request: ProxyRequest,
    pub result: Result<PublishOutcome, ProxyError>,
}

/// A generator supplied by the media/decode layer, kept independent of UI.
pub trait ProxyGenerator: Send + 'static {
    fn generate(
        &mut self,
        request: &ProxyRequest,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProxyError>;
}

impl<F> ProxyGenerator for F
where
    F: FnMut(&ProxyRequest, &CancellationToken) -> Result<Vec<u8>, ProxyError> + Send + 'static,
{
    fn generate(
        &mut self,
        request: &ProxyRequest,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, ProxyError> {
        self(request, cancel)
    }
}

struct QueuedJob {
    sequence: u64,
    request: ProxyRequest,
    token: CancellationToken,
}

impl Clone for QueuedJob {
    fn clone(&self) -> Self {
        Self {
            sequence: self.sequence,
            request: self.request,
            token: self.token.clone(),
        }
    }
}

struct WorkerState {
    stopped: bool,
    current_generation: Option<Generation>,
    latest_sequence: Option<u64>,
    next_sequence: u64,
    queued: Option<QueuedJob>,
    active: Option<QueuedJob>,
}

struct WorkerShared {
    state: Mutex<WorkerState>,
    wake: Condvar,
}

/// A single-slot, latest-request-wins worker.
///
/// There is never more than one pending request.  A newer submission marks
/// the active token cancelled and replaces any queued request.  Before
/// publication the worker re-checks both the token and the request sequence,
/// so an old generator cannot publish after a newer request is accepted.
pub struct ProxyWorker<G> {
    shared: Arc<WorkerShared>,
    cache: Arc<Mutex<ProxyCache>>,
    results: mpsc::Receiver<ProxyCompletion>,
    result_sender: mpsc::SyncSender<ProxyCompletion>,
    worker: Option<JoinHandle<()>>,
    marker: PhantomData<fn() -> G>,
}

impl<G> ProxyWorker<G>
where
    G: ProxyGenerator,
{
    pub fn new(
        cache: Arc<Mutex<ProxyCache>>,
        generator: G,
        completion_capacity: usize,
    ) -> Result<Self, ProxyError> {
        if completion_capacity == 0 {
            return Err(ProxyError::InvalidLimits(
                "proxy completion capacity must be positive".to_owned(),
            ));
        }
        let shared = Arc::new(WorkerShared {
            state: Mutex::new(WorkerState {
                stopped: false,
                current_generation: None,
                latest_sequence: None,
                next_sequence: 0,
                queued: None,
                active: None,
            }),
            wake: Condvar::new(),
        });
        let (result_sender, results) = mpsc::sync_channel(completion_capacity);
        let worker_shared = Arc::clone(&shared);
        let worker_cache = Arc::clone(&cache);
        let worker_results = result_sender.clone();
        let worker = thread::Builder::new()
            .name("slicer-proxy-worker".to_owned())
            .spawn(move || run_worker(worker_shared, worker_cache, worker_results, generator))
            .map_err(|error| ProxyError::Io {
                operation: "start proxy worker".to_owned(),
                path: PathBuf::from("slicer-proxy-worker"),
                message: error.to_string(),
            })?;
        Ok(Self {
            shared,
            cache,
            results,
            result_sender,
            worker: Some(worker),
            marker: PhantomData,
        })
    }

    pub fn new_owned(
        cache: ProxyCache,
        generator: G,
        completion_capacity: usize,
    ) -> Result<Self, ProxyError> {
        Self::new(Arc::new(Mutex::new(cache)), generator, completion_capacity)
    }

    pub fn cache(&self) -> Arc<Mutex<ProxyCache>> {
        Arc::clone(&self.cache)
    }

    pub fn submit(&self, request: ProxyRequest) -> Result<ProxySubmission, ProxyError> {
        request.validate()?;
        let token = CancellationToken::new();
        let mut replaced = None;
        let sequence;
        {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| ProxyError::WorkerStopped)?;
            if state.stopped {
                return Err(ProxyError::WorkerStopped);
            }
            if let Some(current) = state.current_generation
                && request.generation < current
            {
                return Err(ProxyError::StaleGeneration);
            }
            if state
                .current_generation
                .is_none_or(|current| request.generation > current)
            {
                state.current_generation = Some(request.generation);
            }
            sequence = next_sequence(&mut state.next_sequence);
            if let Some(old) = state.queued.replace(QueuedJob {
                sequence,
                request,
                token: token.clone(),
            }) {
                old.token.cancel();
                replaced = Some(old);
            }
            if let Some(active) = &state.active {
                active.token.cancel();
            }
            state.latest_sequence = Some(sequence);
        }
        if let Some(old) = replaced {
            self.try_emit(ProxyCompletion {
                request: old.request,
                result: Err(ProxyError::StaleGeneration),
            });
        }
        self.shared.wake.notify_one();
        Ok(ProxySubmission {
            request,
            sequence,
            token,
        })
    }

    /// Cancel pending/active work without changing the current generation.
    pub fn cancel(&self) {
        let mut queued = None;
        if let Ok(mut state) = self.shared.state.lock() {
            if let Some(job) = state.queued.take() {
                job.token.cancel();
                queued = Some(job);
            }
            if let Some(active) = &state.active {
                active.token.cancel();
            }
        }
        if let Some(job) = queued {
            self.try_emit(ProxyCompletion {
                request: job.request,
                result: Err(ProxyError::Cancelled),
            });
        }
        self.shared.wake.notify_all();
    }

    /// Cancel requests through `generation` inclusively, without making a
    /// newer generation stale.  Use [`Self::advance_generation`] for a seek.
    pub fn cancel_generation(&self, generation: Generation) {
        let mut queued = None;
        if let Ok(mut state) = self.shared.state.lock() {
            if state
                .queued
                .as_ref()
                .is_some_and(|job| job.request.generation <= generation)
                && let Some(job) = state.queued.take()
            {
                job.token.cancel();
                queued = Some(job);
            }
            if let Some(active) = &state.active
                && active.request.generation <= generation
            {
                active.token.cancel();
            }
        }
        if let Some(job) = queued {
            self.try_emit(ProxyCompletion {
                request: job.request,
                result: Err(ProxyError::Cancelled),
            });
        }
        self.shared.wake.notify_all();
    }

    /// Mark a newer generation current and cancel all older work.  Any active
    /// generator is prevented from publishing when it returns.
    pub fn advance_generation(&self, generation: Generation) {
        let mut queued = None;
        if let Ok(mut state) = self.shared.state.lock() {
            let is_newer = state
                .current_generation
                .is_none_or(|current| generation > current);
            if is_newer {
                state.current_generation = Some(generation);
            }
            if state
                .queued
                .as_ref()
                .is_some_and(|job| job.request.generation < generation)
                && let Some(job) = state.queued.take()
            {
                job.token.cancel();
                queued = Some(job);
            }
            if let Some(active) = &state.active
                && active.request.generation < generation
            {
                active.token.cancel();
            }
        }
        if let Some(job) = queued {
            self.try_emit(ProxyCompletion {
                request: job.request,
                result: Err(ProxyError::StaleGeneration),
            });
        }
        self.shared.wake.notify_all();
    }

    pub fn current_generation(&self) -> Option<Generation> {
        self.shared
            .state
            .lock()
            .ok()
            .and_then(|state| state.current_generation)
    }

    pub fn pending_len(&self) -> usize {
        self.shared
            .state
            .lock()
            .map(|state| usize::from(state.queued.is_some()))
            .unwrap_or(0)
    }

    pub fn is_busy(&self) -> bool {
        self.shared
            .state
            .lock()
            .map(|state| state.active.is_some() || state.queued.is_some())
            .unwrap_or(false)
    }

    pub fn try_receive(&self) -> Option<ProxyCompletion> {
        self.results.try_recv().ok()
    }

    pub fn receive_timeout(&self, timeout: std::time::Duration) -> Option<ProxyCompletion> {
        self.results.recv_timeout(timeout).ok()
    }

    fn try_emit(&self, completion: ProxyCompletion) {
        let _ = self.result_sender.try_send(completion);
    }
}

impl<G> Drop for ProxyWorker<G> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.stopped = true;
            if let Some(queued) = &state.queued {
                queued.token.cancel();
            }
            if let Some(active) = &state.active {
                active.token.cancel();
            }
        }
        self.shared.wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn next_sequence(counter: &mut u64) -> u64 {
    *counter = counter.wrapping_add(1);
    if *counter == 0 {
        *counter = 1;
    }
    *counter
}

fn run_worker<G>(
    shared: Arc<WorkerShared>,
    cache: Arc<Mutex<ProxyCache>>,
    results: mpsc::SyncSender<ProxyCompletion>,
    mut generator: G,
) where
    G: ProxyGenerator,
{
    loop {
        let job = {
            let mut state = match shared.state.lock() {
                Ok(state) => state,
                Err(_) => return,
            };
            while !state.stopped && state.queued.is_none() {
                state = match shared.wake.wait(state) {
                    Ok(state) => state,
                    Err(_) => return,
                };
            }
            if state.stopped {
                return;
            }
            let job = state.queued.take().expect("queue checked above");
            state.active = Some(job.clone());
            job
        };

        let generated = generator.generate(&job.request, &job.token);
        let result = {
            let mut state = match shared.state.lock() {
                Ok(state) => state,
                Err(_) => return,
            };
            let current_generation_is_newer = state
                .current_generation
                .is_some_and(|generation| job.request.generation < generation);
            let is_latest = state.latest_sequence == Some(job.sequence);
            let result = if state.stopped || current_generation_is_newer || !is_latest {
                Err(ProxyError::StaleGeneration)
            } else if job.token.is_cancelled() {
                Err(ProxyError::Cancelled)
            } else {
                match generated {
                    Ok(bytes) => match cache.lock() {
                        Ok(mut cache) => {
                            cache.publish_bytes_with_cancel(job.request.key, &bytes, &job.token)
                        }
                        Err(_) => Err(ProxyError::CachePoisoned),
                    },
                    Err(error) => Err(error),
                }
            };
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.sequence == job.sequence)
            {
                state.active = None;
            }
            result
        };
        let _ = results.try_send(ProxyCompletion {
            request: job.request,
            result,
        });
    }
}
