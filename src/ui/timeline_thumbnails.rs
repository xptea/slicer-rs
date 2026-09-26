//! Visible filmstrip frames, decoded off the UI thread with bounded storage.
use super::*;
use slicer::engine::{
    preview_source::PreviewSource,
    project::{Clip, SECOND, Time},
    scrub::cached_proxy,
};
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const MAX_IMAGES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Key(PathBuf, Time);
pub(super) fn poster(path: &Path) -> Key {
    Key(path.to_owned(), 0)
}

pub(super) struct Tile {
    pub x: f32,
    pub width: f32,
    pub key: Key,
}

// Use a source-time grid, not the start of each timeline fragment. A split
// then clips the same tiles at the cut instead of decoding a new filmstrip.
pub(super) fn tiles(
    clip: &Clip,
    offset: Time,
    zoom: f32,
    width: f32,
    tile_width: f32,
) -> Vec<Tile> {
    if clip.graphic.is_some() || !clip.visual || zoom <= 0. || width <= 0. || tile_width <= 0. {
        return Vec::new();
    }
    let left = (clip.start - offset) as f64 / SECOND as f64 * zoom as f64;
    let length = clip.duration as f64 / SECOND as f64 * zoom as f64;
    if left >= width as f64 || left + length <= 0. {
        return Vec::new();
    }
    let step = (tile_width as f64 / zoom as f64 * SECOND as f64)
        .round()
        .max(1.) as Time;
    let visible_source = clip.source_in + ((-left).max(0.) / zoom as f64 * SECOND as f64) as Time;
    let end_source = clip.source_in
        + ((width as f64 - left).min(length) / zoom as f64 * SECOND as f64).ceil() as Time;
    let first = visible_source.div_euclid(step);
    let last = (end_source + step - 1).div_euclid(step);
    (first..last.min(first + MAX_IMAGES as i64))
        .map(|i| {
            let source = i * step;
            let x = (source - clip.source_in) as f64 / SECOND as f64 * zoom as f64;
            Tile {
                x: x as f32,
                width: (step as f64 / SECOND as f64 * zoom as f64).min(length - x) as f32,
                key: Key(
                    clip.path.clone(),
                    if clip.still {
                        0
                    } else {
                        source.clamp(0, (clip.source_duration - 1).max(0))
                    },
                ),
            }
        })
        .collect()
}

pub(super) struct Thumbnails {
    images: HashMap<Key, Option<Arc<RenderImage>>>,
    failed: HashMap<Key, Instant>,
    requested: Vec<Key>,
    tx: mpsc::Sender<(u64, Vec<Key>)>,
    rx: mpsc::Receiver<(Key, Option<Arc<RenderImage>>)>,
    generation: Arc<AtomicU64>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Thumbnails {
    pub fn new() -> Self {
        let (tx, requests) = mpsc::channel::<(u64, Vec<Key>)>();
        let (results, rx) = mpsc::sync_channel(MAX_IMAGES);
        let generation = Arc::new(AtomicU64::new(0));
        let token = generation.clone();
        let worker = thread::spawn(move || {
            let mut decoder: Option<(PathBuf, PreviewSource)> = None;
            while let Ok((mut version, mut keys)) = requests.recv() {
                while let Ok(newer) = requests.try_recv() {
                    (version, keys) = newer;
                }
                if version == u64::MAX {
                    break;
                }
                for key in keys {
                    if token.load(Ordering::Acquire) != version {
                        break;
                    }
                    let result = (|| -> anyhow::Result<Arc<RenderImage>> {
                        let input = cached_proxy(&key.0).unwrap_or_else(|| key.0.clone());
                        if decoder.as_ref().is_none_or(|(path, _)| path != &input) {
                            decoder = Some((input.clone(), PreviewSource::open(&input)?));
                        }
                        let d = &mut decoder.as_mut().unwrap().1;
                        d.cancel_on_generation(token.clone(), version);
                        let frame = d.video(key.1)?;
                        anyhow::ensure!(frame.rgba, "Thumbnail frame must be RGBA");
                        let pixels = image::RgbaImage::from_raw(
                            frame.width,
                            frame.height,
                            frame.pixels.to_vec(),
                        )
                        .ok_or_else(|| anyhow::anyhow!("Invalid thumbnail pixels"))?;
                        let mut pixels = image::DynamicImage::ImageRgba8(pixels)
                            .thumbnail(160, 90)
                            .into_rgba8();
                        for pixel in pixels.pixels_mut() {
                            pixel.0.swap(0, 2);
                        }
                        Ok(Arc::new(RenderImage::new([image::Frame::new(pixels)])))
                    })();
                    if token.load(Ordering::Acquire) != version {
                        // A cancelled FFmpeg session must be reopened for the next plan.
                        decoder = None;
                        break;
                    }
                    if let Err(error) = &result {
                        eprintln!("Timeline thumbnail {}: {error:#}", key.0.display());
                    }
                    if results.send((key, result.ok())).is_err() {
                        return;
                    }
                }
            }
        });
        Self {
            images: HashMap::new(),
            failed: HashMap::new(),
            requested: Vec::new(),
            tx,
            rx,
            generation,
            worker: Some(worker),
        }
    }
    pub fn update(&mut self, mut keys: Vec<Key>) {
        let mut unique = HashSet::new();
        keys.retain(|key| unique.insert(key.clone()));
        keys.truncate(MAX_IMAGES);
        for (key, image) in self.rx.try_iter() {
            // Completed source frames remain useful after trimming or cutting.
            if image.is_some() {
                self.failed.remove(&key);
                self.images.insert(key, image);
            } else {
                self.failed.insert(key, Instant::now());
            }
        }
        let retry = keys.iter().any(|key| {
            self.failed
                .get(key)
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(3))
        });
        if self.requested == keys && !retry {
            return;
        }
        if self.images.len() + keys.len() > MAX_IMAGES {
            self.images.retain(|key, _| keys.contains(key));
        }
        let missing = keys
            .iter()
            .filter(|key| !self.images.contains_key(*key))
            .filter(|key| {
                self.failed
                    .get(*key)
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(3))
            })
            .cloned()
            .collect::<Vec<_>>();
        self.failed
            .retain(|key, _| keys.contains(key) && !missing.contains(key));
        self.requested = keys;
        let version = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.tx.send((version, missing));
    }
    pub fn image(&self, key: &Key) -> Option<Arc<RenderImage>> {
        self.images.get(key).and_then(Clone::clone)
    }
}
impl Drop for Thumbnails {
    fn drop(&mut self) {
        self.generation.store(u64::MAX, Ordering::Release);
        let _ = self.tx.send((u64::MAX, Vec::new()));
        // Wake a blocked result sender before waiting for its GPU teardown.
        let (_, empty) = mpsc::channel();
        drop(std::mem::replace(&mut self.rx, empty));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Key, Thumbnails, tiles};
    use slicer::engine::project::{Clip, SECOND, Transform};
    use std::{
        path::PathBuf,
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn trimmed_filmstrip_keeps_source_frames_when_panned() {
        let clip = Clip {
            id: 1,
            path: PathBuf::from("video.mp4"),
            start: 2 * SECOND,
            source_in: 5 * SECOND,
            duration: 3 * SECOND,
            source_duration: 10 * SECOND,
            visual: true,
            audio: true,
            still: false,
            transform: Transform::default(),
            gain: 1.,
            graphic: None,
            fade_in: 0,
            fade_out: 0,
        };
        let full = tiles(&clip, 0, 60., 600., 72.);
        assert_eq!(
            full.iter().map(|t| t.key.1).collect::<Vec<_>>(),
            [4_800_000, 6_000_000, 7_200_000]
        );
        let panned = tiles(&clip, 3_500_000, 60., 600., 72.);
        assert_eq!(panned[0].key, full[1].key);
        assert_eq!(panned.last().unwrap().width, 48.);
        assert!(tiles(&clip, 6 * SECOND, 60., 600., 72.).is_empty());
    }

    #[test]
    fn splitting_at_an_arbitrary_time_reuses_frames_and_screen_positions() {
        let original = Clip {
            id: 1,
            path: "video.mp4".into(),
            start: 2 * SECOND,
            source_in: 765_432,
            duration: 7 * SECOND,
            source_duration: 10 * SECOND,
            visual: true,
            audio: true,
            still: false,
            transform: Transform::default(),
            gain: 1.,
            graphic: None,
            fade_in: 0,
            fade_out: 0,
        };
        let before = tiles(&original, 0, 61., 1000., 72.);
        let cut = 2_347_123;
        let mut right = original.clone();
        right.start += cut;
        right.source_in += cut;
        right.duration -= cut;
        let mut left = original.clone();
        left.duration = cut;
        for clip in [&left, &right] {
            for tile in tiles(clip, 0, 61., 1000., 72.) {
                let prior = before
                    .iter()
                    .find(|t| t.key == tile.key)
                    .expect("cut requested a new decode");
                let screen = clip.start as f32 / SECOND as f32 * 61. + tile.x;
                let old_screen = original.start as f32 / SECOND as f32 * 61. + prior.x;
                assert!((screen - old_screen).abs() < 0.001);
            }
        }
    }

    #[test]
    fn background_thumbnail_has_correct_channels_and_reuses_stills() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("red.png");
        image::RgbaImage::from_pixel(24, 48, image::Rgba([255, 0, 0, 255]))
            .save(&path)
            .unwrap();
        let key = Key(path, 0);
        let mut thumbnails = Thumbnails::new();
        let start = Instant::now();
        loop {
            thumbnails.update(vec![key.clone(), key.clone()]);
            if let Some(image) = thumbnails.image(&key) {
                assert_eq!(&image.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]);
                assert_eq!(thumbnails.requested.len(), 1);
                assert_eq!(thumbnails.images.len(), 1);
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "Thumbnail was never decoded"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "Requires GPU playback runtime and SLICER_THUMBNAIL_TEST_FILE"]
    fn hdr_filmstrip_finishes_and_cut_frames_stay_cached() {
        let path = PathBuf::from(std::env::var_os("SLICER_THUMBNAIL_TEST_FILE").unwrap());
        let keys: Vec<_> = [0, 1_500_000, 12_000_000, 25_000_000]
            .into_iter()
            .map(|t| Key(path.clone(), t))
            .collect();
        let mut thumbnails = Thumbnails::new();
        let began = Instant::now();
        loop {
            thumbnails.update(keys.clone());
            if keys.iter().all(|key| thumbnails.image(key).is_some()) {
                break;
            }
            assert!(
                began.elapsed() < Duration::from_secs(30),
                "HDR filmstrip did not finish"
            );
            thread::sleep(Duration::from_millis(10));
        }
        println!("HDR filmstrip: four cold samples in {:?}", began.elapsed());
        let before = thumbnails.image(&keys[1]).unwrap();
        thumbnails.update(vec![keys[1].clone(), keys[3].clone()]);
        assert!(std::sync::Arc::ptr_eq(
            &before,
            &thumbnails.image(&keys[1]).unwrap()
        ));
        let pixels = before.as_bytes(0).unwrap();
        assert!(pixels.iter().max().unwrap() - pixels.iter().min().unwrap() > 32);
    }
}
