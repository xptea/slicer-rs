//! Legacy single-video backend adapter.
//!
//! The adapter keeps the old player behind the same tagged transport API.  It
//! owns only a backend port and the minimum transport bookkeeping needed to
//! reject stale commands; it does not own a window, GPUI state, project edits,
//! or timeline selection.  An integration layer can implement
//! [`LegacyPlayerPort`] for the current `NativePlayer` without changing that
//! player or this contract file.

use super::api::{
    BackendError, PlaybackBackend, PlaybackCommand, PlaybackCommandKind, PlaybackEvent,
    PlaybackEventKind, PlaybackState, PlaybackStatistics, WorkTag,
};
use crate::project::{Rational, Time, TimeRange};
use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::path::Path;

/// Snapshot shape required from a legacy player.  The current libmpv player
/// already publishes these values through its non-blocking `snapshot()` call.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LegacySnapshot {
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub seeking: bool,
    pub loaded: bool,
    pub eof: bool,
    pub muted: bool,
    pub error: Option<String>,
}

/// Minimal port for an existing asynchronous single-video player.
pub trait LegacyPlayerPort: Send + Sync {
    fn load_file(&self, path: &Path, start: f64, end: Option<f64>) -> Result<(), String>;
    fn set_range(&self, start: f64, end: f64) -> Result<(), String>;
    fn set_paused(&self, paused: bool) -> Result<(), String>;
    fn set_mute(&self, muted: bool) -> Result<(), String>;
    fn seek(&self, seconds: f64, exact: bool) -> Result<(), String>;
    fn snapshot(&self) -> LegacySnapshot;

    /// Current `NativePlayer` teardown is driven by its owner.  A port may
    /// override this if the wrapped backend has an explicit asynchronous stop.
    fn shutdown(&self) {}
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LegacyAdapterError {
    InvalidTime,
    InvalidRange,
}

impl fmt::Display for LegacyAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTime => formatter.write_str("legacy position is not finite"),
            Self::InvalidRange => {
                formatter.write_str("legacy range is outside the loaded duration")
            }
        }
    }
}

impl Error for LegacyAdapterError {}

/// A backend adapter around a legacy player port.
pub struct LegacyBackendAdapter<P> {
    player: P,
    current_tag: Option<WorkTag>,
    duration: Option<Time>,
    range: Option<TimeRange>,
    last_snapshot: Option<LegacySnapshot>,
    pending_events: VecDeque<PlaybackEvent>,
    stopped: bool,
}

impl<P> LegacyBackendAdapter<P>
where
    P: LegacyPlayerPort,
{
    pub fn new(player: P) -> Self {
        Self {
            player,
            current_tag: None,
            duration: None,
            range: None,
            last_snapshot: None,
            pending_events: VecDeque::new(),
            stopped: false,
        }
    }

    pub fn player(&self) -> &P {
        &self.player
    }

    pub fn current_tag(&self) -> Option<WorkTag> {
        self.current_tag
    }

    pub fn into_player(self) -> P {
        self.player
    }

    fn admit(&mut self, tag: WorkTag, load: bool) -> Result<(), BackendError> {
        if self.stopped {
            return Err(BackendError::Shutdown);
        }
        if let Some(current) = self.current_tag {
            if !load && tag.revision != current.revision {
                return Err(BackendError::StaleCommand);
            }
            if tag.revision == current.revision && tag.generation < current.generation {
                return Err(BackendError::StaleCommand);
            }
        }
        self.current_tag = Some(tag);
        Ok(())
    }

    fn backend_failure(error: String) -> BackendError {
        BackendError::Failed(error)
    }

    fn emit_error(&mut self, tag: WorkTag, error: BackendError) {
        self.pending_events.push_back(PlaybackEvent::new(
            tag,
            PlaybackEventKind::Error {
                code: super::api::EventErrorCode::Backend,
                message: error.to_string(),
                recoverable: true,
            },
        ));
    }

    fn tag(&self) -> Option<WorkTag> {
        self.current_tag
    }

    fn apply_load(
        &mut self,
        tag: WorkTag,
        path: &Path,
        duration: Time,
        range: Option<TimeRange>,
    ) -> Result<(), BackendError> {
        if path.as_os_str().is_empty() || duration < Time::ZERO {
            return Err(BackendError::InvalidCommand(
                "legacy load requires a non-empty path and non-negative duration".to_owned(),
            ));
        }
        if let Some(range) = range {
            validate_range(range, duration).map_err(|_| {
                BackendError::InvalidCommand("legacy load range is outside duration".to_owned())
            })?;
        }
        self.admit(tag, true)?;
        let start = range.map_or(Time::ZERO, |range| range.start).to_f64();
        let result = self
            .player
            .load_file(path, start, range.map(|range| range.end.to_f64()));
        if let Err(error) = result {
            self.emit_error(tag, Self::backend_failure(error.clone()));
            return Err(Self::backend_failure(error));
        }
        self.duration = Some(duration);
        self.range = range;
        self.last_snapshot = None;
        self.pending_events.push_back(PlaybackEvent::new(
            tag,
            PlaybackEventKind::Ready { duration, range },
        ));
        Ok(())
    }
}

impl<P> PlaybackBackend for LegacyBackendAdapter<P>
where
    P: LegacyPlayerPort,
{
    fn submit(&mut self, command: PlaybackCommand) -> Result<(), BackendError> {
        let tag = command.tag;
        match command.kind {
            PlaybackCommandKind::Load {
                path,
                duration,
                range,
            } => self.apply_load(tag, &path, duration, range),
            PlaybackCommandKind::Play => {
                self.admit(tag, false)?;
                self.player.set_paused(false).map_err(Self::backend_failure)
            }
            PlaybackCommandKind::Pause => {
                self.admit(tag, false)?;
                self.player.set_paused(true).map_err(Self::backend_failure)
            }
            PlaybackCommandKind::Seek { time, exact } => {
                self.admit(tag, false)?;
                let Some(duration) = self.duration else {
                    return Err(BackendError::InvalidCommand(
                        "legacy seek requires a loaded file".to_owned(),
                    ));
                };
                let range = self
                    .range
                    .unwrap_or(TimeRange::new(Time::ZERO, duration).map_err(|_| {
                        BackendError::InvalidCommand("legacy duration is empty".to_owned())
                    })?);
                let target = time.max(range.start).min(range.end);
                self.player
                    .seek(target.to_f64(), exact)
                    .map_err(Self::backend_failure)
            }
            PlaybackCommandKind::SetRange { range } => {
                self.admit(tag, false)?;
                let Some(duration) = self.duration else {
                    return Err(BackendError::InvalidCommand(
                        "legacy range requires a loaded file".to_owned(),
                    ));
                };
                let range =
                    range.unwrap_or(TimeRange::new(Time::ZERO, duration).map_err(|_| {
                        BackendError::InvalidCommand("legacy duration is empty".to_owned())
                    })?);
                validate_range(range, duration).map_err(|_| {
                    BackendError::InvalidCommand("legacy range is outside duration".to_owned())
                })?;
                self.player
                    .set_range(range.start.to_f64(), range.end.to_f64())
                    .map_err(Self::backend_failure)?;
                self.range = Some(range);
                Ok(())
            }
            PlaybackCommandKind::SetMonitorMute { muted } => {
                self.admit(tag, false)?;
                self.player.set_mute(muted).map_err(Self::backend_failure)
            }
            PlaybackCommandKind::Resize { .. } => {
                // Surface geometry is deliberately owned by the UI/native
                // surface integration, not by this transport adapter.
                self.admit(tag, false)
            }
            PlaybackCommandKind::RequestDiagnostics => {
                self.admit(tag, false)?;
                let snapshot = self.player.snapshot();
                self.pending_events.push_back(PlaybackEvent::new(
                    tag,
                    PlaybackEventKind::Statistics(PlaybackStatistics {
                        queued_commands: 0,
                        ..PlaybackStatistics::default()
                    }),
                ));
                self.last_snapshot = Some(snapshot);
                Ok(())
            }
            PlaybackCommandKind::Shutdown => {
                if !self.stopped {
                    self.player.shutdown();
                    self.stopped = true;
                    self.pending_events
                        .push_back(PlaybackEvent::new(tag, PlaybackEventKind::Shutdown));
                }
                Ok(())
            }
        }
    }

    fn try_event(&mut self) -> Option<PlaybackEvent> {
        if let Some(event) = self.pending_events.pop_front() {
            return Some(event);
        }
        let tag = self.tag()?;
        let snapshot = self.player.snapshot();
        if self.last_snapshot.as_ref() == Some(&snapshot) {
            return None;
        }
        self.last_snapshot = Some(snapshot.clone());
        if let Some(error) = snapshot.error {
            return Some(PlaybackEvent::new(
                tag,
                PlaybackEventKind::Error {
                    code: super::api::EventErrorCode::Backend,
                    message: error,
                    recoverable: true,
                },
            ));
        }
        let position = match Rational::from_seconds(snapshot.position) {
            Ok(position) if position >= Time::ZERO => position,
            _ => {
                return Some(PlaybackEvent::new(
                    tag,
                    PlaybackEventKind::Error {
                        code: super::api::EventErrorCode::Backend,
                        message: LegacyAdapterError::InvalidTime.to_string(),
                        recoverable: true,
                    },
                ));
            }
        };
        let state = if snapshot.eof {
            PlaybackState::Ended
        } else if snapshot.paused {
            PlaybackState::Paused
        } else {
            PlaybackState::Playing
        };
        Some(PlaybackEvent::new(
            tag,
            PlaybackEventKind::CurrentTime {
                time: position,
                state,
                eof: snapshot.eof,
            },
        ))
    }
}

fn validate_range(range: TimeRange, duration: Time) -> Result<(), LegacyAdapterError> {
    if range.start < Time::ZERO || range.end > duration || range.end <= range.start {
        return Err(LegacyAdapterError::InvalidRange);
    }
    Ok(())
}
