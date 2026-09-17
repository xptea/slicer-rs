//! P11 proxy/cache contracts are also included by path so this test exercises
//! the implementation in isolation as well as through the public engine API.

#![allow(dead_code)]

mod project {
    pub use slicer::project::*;
}

#[path = "../src/engine/proxy/mod.rs"]
mod proxy;

use proxy::{
    CacheLimits, Generation, ProxyCache, ProxyCompletion, ProxyError, ProxyFormat, ProxyGenerator,
    ProxyKey, ProxyRequest, ProxySpec, ProxyWorker, PublishStatus, deterministic_cache_path,
};
use slicer::project::{AssetId, Time};
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

fn key(asset: u64, transform_revision: u64, time: Time) -> ProxyKey {
    ProxyKey::new(
        AssetId::new(asset),
        17,
        time,
        transform_revision,
        ProxySpec::with_format(320, 180, ProxyFormat::Rgba8, 70),
    )
}

fn request(asset: u64, generation: Generation) -> ProxyRequest {
    ProxyRequest::new(key(asset, 1, Time::new(1, 2).unwrap()), generation)
}

fn wait_until(predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "condition was not reached in time"
        );
        thread::yield_now();
    }
}

fn receive_result<G>(worker: &ProxyWorker<G>) -> ProxyCompletion
where
    G: ProxyGenerator,
{
    worker
        .receive_timeout(Duration::from_secs(2))
        .expect("proxy worker completion")
}

#[test]
fn proxy_keys_and_paths_are_stable_for_canonical_time() {
    let root = tempfile::tempdir().unwrap();
    let first = key(9, 3, Time::new(2, 4).unwrap());
    let equivalent = key(9, 3, Time::new(1, 2).unwrap());
    let changed_transform = key(9, 4, Time::new(1, 2).unwrap());

    assert_eq!(first, equivalent);
    assert_eq!(first.canonical_bytes(), equivalent.canonical_bytes());
    assert_eq!(first.stable_token(), equivalent.stable_token());
    assert_eq!(
        deterministic_cache_path(root.path(), &first),
        deterministic_cache_path(root.path(), &equivalent)
    );
    assert_ne!(first.stable_token(), changed_transform.stable_token());
    assert_ne!(
        deterministic_cache_path(root.path(), &first),
        deterministic_cache_path(root.path(), &changed_transform)
    );
}

#[test]
fn disk_budget_accounts_bytes_and_evicts_lru_entries() {
    let directory = tempfile::tempdir().unwrap();
    let mut cache = ProxyCache::new(directory.path(), CacheLimits::new(8, 2, 6)).unwrap();
    let first = key(1, 1, Time::ZERO);
    let second = key(2, 1, Time::ZERO);
    let third = key(3, 1, Time::ZERO);

    assert_eq!(
        cache.publish_bytes(first, b"1111").unwrap().status,
        PublishStatus::Published
    );
    assert_eq!(
        cache.publish_bytes(second, b"2222").unwrap().status,
        PublishStatus::Published
    );
    assert!(
        cache.get(&first).unwrap().is_some(),
        "first entry should be a hit"
    );

    let outcome = cache.publish_bytes(third, b"3333").unwrap();
    assert_eq!(outcome.status, PublishStatus::Published);
    assert_eq!(outcome.evicted_entries, 1);
    assert_eq!(outcome.evicted_bytes, 4);
    assert!(cache.contains(first));
    assert!(!cache.contains(second));
    assert!(cache.contains(third));
    assert_eq!(cache.len(), 2);
    assert_eq!(cache.bytes(), 8);

    let rejected = cache
        .publish_bytes(key(4, 1, Time::ZERO), b"1234567")
        .unwrap();
    assert_eq!(rejected.status, PublishStatus::RejectedOversize);
    assert_eq!(cache.bytes(), 8);
    assert_eq!(cache.len(), 2);
}

#[test]
fn stale_generation_cannot_publish_after_a_newer_request() {
    let directory = tempfile::tempdir().unwrap();
    let started = Arc::new(AtomicBool::new(false));
    let started_by_generator = Arc::clone(&started);
    let generator = move |request: &ProxyRequest,
                          cancel: &proxy::CancellationToken|
          -> Result<Vec<u8>, ProxyError> {
        if request.generation == 1 {
            started_by_generator.store(true, Ordering::Release);
            while !cancel.is_cancelled() {
                thread::yield_now();
            }
            return Err(ProxyError::Cancelled);
        }
        Ok(vec![request.generation as u8; 4])
    };
    let cache = ProxyCache::new(directory.path(), CacheLimits::new(32, 4, 16)).unwrap();
    let worker = ProxyWorker::new_owned(cache, generator, 8).unwrap();
    let old = request(1, 1);
    let newer = request(2, 2);

    worker.submit(old).unwrap();
    wait_until(|| started.load(Ordering::Acquire));
    worker.submit(newer).unwrap();

    let old_completion = receive_result(&worker);
    assert_eq!(old_completion.request, old);
    assert_eq!(old_completion.result, Err(ProxyError::StaleGeneration));

    let new_completion = receive_result(&worker);
    assert_eq!(new_completion.request, newer);
    assert_eq!(
        new_completion.result.as_ref().unwrap().status,
        PublishStatus::Published
    );
    let cache = worker.cache();
    let cache = cache.lock().unwrap();
    assert!(!cache.contains(old.key));
    assert!(cache.contains(newer.key));
}

#[test]
fn explicit_cancellation_stops_generation_and_leaves_no_artifact() {
    let directory = tempfile::tempdir().unwrap();
    let started = Arc::new(AtomicBool::new(false));
    let started_by_generator = Arc::clone(&started);
    let generator =
        move |_: &ProxyRequest, cancel: &proxy::CancellationToken| -> Result<Vec<u8>, ProxyError> {
            started_by_generator.store(true, Ordering::Release);
            while !cancel.is_cancelled() {
                thread::yield_now();
            }
            Err(ProxyError::Cancelled)
        };
    let cache = ProxyCache::new(directory.path(), CacheLimits::new(32, 4, 16)).unwrap();
    let worker = ProxyWorker::new_owned(cache, generator, 4).unwrap();
    let request = request(5, 0);
    let submission = worker.submit(request).unwrap();
    wait_until(|| started.load(Ordering::Acquire));
    submission.cancel();

    let completion = receive_result(&worker);
    assert_eq!(completion.request, request);
    assert_eq!(completion.result, Err(ProxyError::Cancelled));
    let cache = worker.cache();
    let cache = cache.lock().unwrap();
    assert!(!cache.contains(request.key));
    assert!(fs::read_dir(directory.path()).unwrap().next().is_none());
}

#[test]
fn competing_publishers_have_atomic_no_overwrite_destination_race() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let key = key(21, 9, Time::new(3, 5).unwrap());
    let start = Arc::new(Barrier::new(2));
    let limits = CacheLimits::new(4, 1, 4);

    let mut threads = Vec::new();
    for value in [b"AAAA".to_vec(), b"BBBB".to_vec()] {
        let root = root.clone();
        let start = Arc::clone(&start);
        threads.push(thread::spawn(move || {
            let mut cache = ProxyCache::new(root, limits).unwrap();
            start.wait();
            cache.publish_bytes(key, &value).unwrap()
        }));
    }
    let outcomes = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.status == PublishStatus::Published)
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.status == PublishStatus::AlreadyPresent)
            .count(),
        1
    );
    let published_path = deterministic_cache_path(directory.path(), &key);
    let contents = fs::read(published_path).unwrap();
    assert!(contents == b"AAAA" || contents == b"BBBB");
}
