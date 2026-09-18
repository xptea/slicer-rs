use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Integer microseconds. Source frame selection still uses its actual PTS.
pub type Time = i64;
pub const SECOND: Time = 1_000_000;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Transform {
    /// Center in normalized project coordinates; dimensions relative to project size.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub rotation: f32,
    pub opacity: f32,
}
impl Default for Transform {
    fn default() -> Self {
        Self {
            x: 0.5,
            y: 0.5,
            width: 1.,
            height: 1.,
            rotation: 0.,
            opacity: 1.,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Clip {
    pub id: u64,
    pub path: PathBuf,
    pub start: Time,
    pub source_in: Time,
    pub duration: Time,
    pub source_duration: Time,
    pub visual: bool,
    pub audio: bool,
    pub still: bool,
    pub transform: Transform,
    pub gain: f32,
}
impl Clip {
    pub fn end(&self) -> Time {
        self.start.saturating_add(self.duration)
    }
    pub fn source_time(&self, time: Time) -> Option<Time> {
        (time >= self.start && time < self.end()).then(|| {
            if self.still {
                0
            } else {
                self.source_in + time - self.start
            }
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub name: String,
    pub muted: bool,
    pub hidden: bool,
    pub locked: bool,
    pub clips: Vec<Clip>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub version: u32,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub tracks: Vec<Track>,
    pub next_id: u64,
}
impl Default for Project {
    fn default() -> Self {
        Self {
            version: 1,
            width: 1920,
            height: 1080,
            fps_num: 30,
            fps_den: 1,
            tracks: vec![
                Track::new("Video 1"),
                Track::new("Video 2"),
                Track::new("Audio"),
            ],
            next_id: 1,
        }
    }
}
impl Track {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            muted: false,
            hidden: false,
            locked: false,
            clips: vec![],
        }
    }
}
impl Project {
    pub fn duration(&self) -> Time {
        self.tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(Clip::end)
            .max()
            .unwrap_or(0)
    }
    pub fn clip(&self, id: u64) -> Option<&Clip> {
        self.tracks
            .iter()
            .flat_map(|t| &t.clips)
            .find(|c| c.id == id)
    }
    pub fn clip_mut(&mut self, id: u64) -> Option<&mut Clip> {
        self.tracks
            .iter_mut()
            .flat_map(|t| &mut t.clips)
            .find(|c| c.id == id)
    }
    pub fn editable(&self, id: u64) -> bool {
        self.tracks
            .iter()
            .any(|t| !t.locked && t.clips.iter().any(|c| c.id == id))
    }
    /// Tracks in bottom-to-top composition order. Audio is evaluated independently of visibility.
    pub fn active(&self, time: Time) -> impl Iterator<Item = (&Track, &Clip, Time)> {
        self.tracks.iter().flat_map(move |t| {
            t.clips
                .iter()
                .filter_map(move |c| c.source_time(time).map(|s| (t, c, s)))
        })
    }
    pub fn split(&mut self, id: u64, at: Time) -> bool {
        if !self.editable(id) {
            return false;
        }
        let Some(c) = self.clip(id) else { return false };
        if at <= c.start || at >= c.end() {
            return false;
        }
        let mut right = c.clone();
        let delta = at - c.start;
        right.id = self.next_id;
        self.next_id += 1;
        right.start = at;
        right.duration -= delta;
        if !right.still {
            right.source_in += delta;
        }
        let track = self
            .tracks
            .iter_mut()
            .find(|t| t.clips.iter().any(|c| c.id == id))
            .unwrap();
        track
            .clips
            .iter_mut()
            .find(|c| c.id == id)
            .unwrap()
            .duration = delta;
        track.clips.push(right);
        true
    }
    pub fn move_clip(&mut self, id: u64, track: usize, start: Time) -> bool {
        if track >= self.tracks.len() || self.tracks[track].locked || !self.editable(id) {
            return false;
        }
        let from = self
            .tracks
            .iter()
            .position(|t| t.clips.iter().any(|c| c.id == id))
            .unwrap();
        let index = self.tracks[from]
            .clips
            .iter()
            .position(|c| c.id == id)
            .unwrap();
        let mut c = self.tracks[from].clips.remove(index);
        c.start = start.max(0);
        self.tracks[track].clips.push(c);
        true
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || self.fps_num == 0
            || self.fps_den == 0
            || self.fps_num > 240_000
            || self.fps_den > 10_000
            || self.fps_num as f64 / self.fps_den as f64 > 240.
            || self.duration() > 24 * 3600 * SECOND
        {
            bail!("Unsupported project settings");
        }
        let mut ids = std::collections::HashSet::new();
        for c in self.tracks.iter().flat_map(|t| &t.clips) {
            let x = &c.transform;
            if !ids.insert(c.id)
                || c.id >= self.next_id
                || c.start < 0
                || c.source_in < 0
                || c.duration <= 0
                || c.start.checked_add(c.duration).is_none()
                || c.source_in.checked_add(c.duration).is_none()
                || (!c.still && c.source_in + c.duration > c.source_duration)
            {
                bail!("Invalid timing or ID for clip {}", c.id);
            }
            if ![x.x, x.y, x.width, x.height, x.rotation, x.opacity, c.gain]
                .iter()
                .all(|v| v.is_finite())
                || x.width <= 0.
                || x.height <= 0.
                || !(0. ..=1.).contains(&x.opacity)
                || !(0. ..=4.).contains(&c.gain)
            {
                bail!("Invalid transform or gain");
            }
        }
        Ok(())
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        let result = (|| {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            f.write_all(&serde_json::to_vec_pretty(self)?)?;
            f.sync_all()?;
            std::fs::rename(&temp, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
    pub fn load(path: &Path) -> Result<Self> {
        let mut p: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        p.validate()?;
        for c in p.tracks.iter_mut().flat_map(|t| &mut t.clips) {
            if c.path.is_relative() {
                c.path = path.parent().unwrap_or(Path::new(".")).join(&c.path);
            }
            c.path.try_exists().context("Check media path")?;
        }
        Ok(p)
    }
}
#[derive(Default)]
pub struct History {
    undo: Vec<Project>,
    redo: Vec<Project>,
}
impl History {
    pub fn checkpoint(&mut self, p: &Project) {
        self.undo.push(p.clone());
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }
    pub fn undo(&mut self, p: &mut Project) {
        if let Some(old) = self.undo.pop() {
            self.redo.push(std::mem::replace(p, old));
        }
    }
    pub fn redo(&mut self, p: &mut Project) {
        if let Some(old) = self.redo.pop() {
            self.undo.push(std::mem::replace(p, old));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Project {
        let mut p = Project::default();
        p.next_id = 2;
        p.tracks[0].clips.push(Clip {
            id: 1,
            path: "a.mp4".into(),
            start: SECOND,
            source_in: 2 * SECOND,
            duration: 4 * SECOND,
            source_duration: 8 * SECOND,
            visual: true,
            audio: true,
            still: false,
            transform: Transform::default(),
            gain: 1.,
        });
        p
    }
    #[test]
    fn split_preserves_source_and_exclusive_bounds() {
        let mut p = fixture();
        assert!(p.split(1, 3 * SECOND));
        assert_eq!(
            p.active(3 * SECOND)
                .map(|(_, c, s)| (c.id, s))
                .collect::<Vec<_>>(),
            vec![(2, 4 * SECOND)]
        );
        assert_eq!(p.duration(), 5 * SECOND);
        p.validate().unwrap();
    }
    #[test]
    fn move_and_undo() {
        let mut p = fixture();
        let original = p.clone();
        let mut h = History::default();
        h.checkpoint(&p);
        assert!(p.move_clip(1, 1, 0));
        assert_eq!(p.clip(1).unwrap().source_in, 2 * SECOND);
        h.undo(&mut p);
        assert_eq!(p, original);
        h.redo(&mut p);
        assert_eq!(p.clip(1).unwrap().start, 0);
    }
    #[test]
    fn locked_and_invalid_projects() {
        let mut p = fixture();
        p.tracks[0].locked = true;
        assert!(!p.split(1, 2 * SECOND));
        p.clip_mut(1).unwrap().transform.opacity = f32::NAN;
        assert!(p.validate().is_err());
    }
}
