//! One background extractor shared by all timeline audio sources.
use super::*;
use std::collections::{HashMap, HashSet};

pub(super) struct Waveforms {
    worker: Option<waveform::WaveformWorker>,
    pending: Option<(PathBuf, u64)>,
    cache: HashMap<PathBuf, Option<Arc<waveform::Waveform>>>,
}
impl Waveforms {
    pub fn new() -> Self {
        Self {
            worker: media::Binaries::resolve()
                .ok()
                .map(waveform::WaveformWorker::new),
            pending: None,
            cache: HashMap::new(),
        }
    }
    pub fn update(&mut self, sources: Vec<(PathBuf, f64)>) {
        let Some(worker) = &self.worker else {
            return;
        };
        let needed: HashSet<_> = sources.iter().map(|(path, _)| path).collect();
        self.cache.retain(|path, _| needed.contains(path));
        if self
            .pending
            .as_ref()
            .is_some_and(|(path, _)| !needed.contains(path))
        {
            worker.cancel();
            self.pending = None;
        }
        for event in worker.events.try_iter() {
            if self.pending.as_ref() != Some(&(event.path.clone(), event.generation)) {
                continue;
            }
            self.pending = None;
            self.cache
                .insert(event.path, event.result.ok().map(Arc::new));
        }
        if self.pending.is_none() {
            if let Some((path, duration)) = sources
                .into_iter()
                .find(|(path, _)| !self.cache.contains_key(path))
            {
                let generation = worker.request(path.clone(), duration, true);
                self.pending = Some((path, generation));
            }
        }
    }
    pub fn get(&self, path: &Path) -> Option<Arc<waveform::Waveform>> {
        self.cache.get(path).and_then(Clone::clone)
    }
}

#[cfg(test)]
mod tests {
    use super::Waveforms;
    use slicer::engine::decoder::Decoder;
    use std::{
        thread,
        time::{Duration, Instant},
    };

    #[test]
    #[ignore = "Requires bundled FFmpeg and SLICER_WAVEFORM_TEST_FILES"]
    fn every_audio_source_finishes_and_removed_sources_are_released() {
        let paths: Vec<_> =
            std::env::split_paths(&std::env::var_os("SLICER_WAVEFORM_TEST_FILES").unwrap())
                .collect();
        let sources: Vec<_> = paths
            .iter()
            .map(|p| {
                (
                    p.clone(),
                    Decoder::open(p, false).unwrap().info.duration_us as f64 / 1e6,
                )
            })
            .collect();
        let mut waves = Waveforms::new();
        let start = Instant::now();
        loop {
            waves.update(sources.clone());
            if paths.iter().all(|p| waves.get(p).is_some()) {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(35),
                "Audio source did not produce a waveform"
            );
            thread::sleep(Duration::from_millis(20));
        }
        for path in &paths {
            let wave = waves.get(path).unwrap();
            assert!(wave.peaks().iter().any(|p| p.max > 0.01 || p.min < -0.01));
        }
        waves.update(Vec::new());
        assert!(waves.cache.is_empty());
        assert!(waves.pending.is_none());
    }
}
