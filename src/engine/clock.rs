//! Exact project playback clock.
//!
//! The clock has one authoritative project position.  In silent preview it
//! advances from a monotonic [`Instant`].  With an audio device it advances
//! from the number of samples consumed by that device, minus the configured
//! output latency.  Both paths use the project's rational `Time`, so pause,
//! range boundaries, seek, and EOF do not depend on floating-point rounding.

use super::api::Generation;
pub use super::api::PlaybackState;
use crate::project::{RationalError, Time, TimeRange};
use std::error::Error;
use std::fmt;
use std::time::{Duration, Instant};

/// How the project position is advanced while playing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockMode {
    /// Advance from a monotonic wall clock.  This is used for silent projects
    /// and when an audio device is unavailable.
    Silent,
    /// Advance only from consumed audio samples.  Wall time is not allowed to
    /// make the video run ahead of audio.
    AudioDriven,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClockBounds {
    start: Time,
    end: Time,
}

impl ClockBounds {
    fn range(self) -> Option<TimeRange> {
        TimeRange::new(self.start, self.end).ok()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AudioClock {
    sample_rate: u32,
    latency_samples: u64,
    anchor_consumed: u64,
    last_consumed: u64,
}

/// A read-only observation of the clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockSnapshot {
    pub position: Time,
    pub duration: Time,
    pub range: Option<TimeRange>,
    pub state: PlaybackState,
    pub eof: bool,
    pub mode: ClockMode,
    pub generation: Generation,
}

/// Project clock errors.  They are explicit so a backend can surface a
/// recoverable device/counter problem instead of silently drifting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClockError {
    NegativeDuration,
    InvalidRange,
    RangeOutsideDuration,
    Time(RationalError),
    AudioNotConfigured,
    AudioClockDisabled,
    AudioCounterRewound,
    InvalidAudioRate,
}

impl fmt::Display for ClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NegativeDuration => formatter.write_str("clock duration must be non-negative"),
            Self::InvalidRange => formatter.write_str("clock range must be non-empty and ordered"),
            Self::RangeOutsideDuration => {
                formatter.write_str("clock range must be inside the project duration")
            }
            Self::Time(error) => error.fmt(formatter),
            Self::AudioNotConfigured => formatter.write_str("audio clock is not configured"),
            Self::AudioClockDisabled => formatter.write_str("clock is not audio-driven"),
            Self::AudioCounterRewound => {
                formatter.write_str("audio consumption counter moved backwards")
            }
            Self::InvalidAudioRate => formatter.write_str("audio sample rate must be positive"),
        }
    }
}

impl Error for ClockError {}

impl From<RationalError> for ClockError {
    fn from(error: RationalError) -> Self {
        Self::Time(error)
    }
}

/// A single exact project clock owned by the playback worker.
pub struct ProjectClock {
    duration: Time,
    bounds: ClockBounds,
    position: Time,
    state: PlaybackState,
    mode: ClockMode,
    eof: bool,
    generation: Generation,
    monotonic_anchor: Option<Instant>,
    position_anchor: Time,
    audio: Option<AudioClock>,
}

impl ProjectClock {
    /// Construct a paused clock.  `None` selects the full project range.
    pub fn new(
        duration: Time,
        range: Option<TimeRange>,
        mode: ClockMode,
    ) -> Result<Self, ClockError> {
        Self::new_at(duration, range, mode, Instant::now())
    }

    /// Deterministic constructor for tests and offline consumers.
    pub fn new_at(
        duration: Time,
        range: Option<TimeRange>,
        mode: ClockMode,
        _now: Instant,
    ) -> Result<Self, ClockError> {
        if duration < Time::ZERO {
            return Err(ClockError::NegativeDuration);
        }
        let bounds = match range {
            Some(range) => {
                if range.start < Time::ZERO {
                    return Err(ClockError::InvalidRange);
                }
                if range.end > duration {
                    return Err(ClockError::RangeOutsideDuration);
                }
                ClockBounds {
                    start: range.start,
                    end: range.end,
                }
            }
            None => ClockBounds {
                start: Time::ZERO,
                end: duration,
            },
        };
        if bounds.end <= bounds.start && duration > Time::ZERO {
            return Err(ClockError::InvalidRange);
        }
        let ended = bounds.end == bounds.start;
        Ok(Self {
            duration,
            bounds,
            position: bounds.start,
            state: if ended {
                PlaybackState::Ended
            } else {
                PlaybackState::Paused
            },
            mode,
            eof: ended,
            generation: 0,
            monotonic_anchor: None,
            position_anchor: bounds.start,
            audio: None,
        })
    }

    pub fn snapshot(&self) -> ClockSnapshot {
        ClockSnapshot {
            position: self.position,
            duration: self.duration,
            range: self.bounds.range(),
            state: self.state,
            eof: self.eof,
            mode: self.mode,
            generation: self.generation,
        }
    }

    pub fn position(&self) -> Time {
        self.position
    }

    pub fn duration(&self) -> Time {
        self.duration
    }

    pub fn range(&self) -> Option<TimeRange> {
        self.bounds.range()
    }

    pub fn state(&self) -> PlaybackState {
        self.state
    }

    pub fn mode(&self) -> ClockMode {
        self.mode
    }

    pub fn eof(&self) -> bool {
        self.eof
    }

    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// Advance the clock at a monotonic observation.  Audio-driven clocks do
    /// not advance here; call [`Self::on_audio_consumed`] instead.
    pub fn tick_at(&mut self, now: Instant) -> Result<ClockSnapshot, ClockError> {
        if self.state != PlaybackState::Playing || self.mode != ClockMode::Silent {
            return Ok(self.snapshot());
        }
        let Some(anchor) = self.monotonic_anchor else {
            return Ok(self.snapshot());
        };
        let elapsed = now.saturating_duration_since(anchor);
        let delta = duration_to_time(elapsed)?;
        let candidate = self.position_anchor.checked_add(delta)?;
        self.apply_position(candidate);
        Ok(self.snapshot())
    }

    pub fn tick(&mut self) -> Result<ClockSnapshot, ClockError> {
        self.tick_at(Instant::now())
    }

    /// Deterministically advance a silent playing clock without sleeping.
    pub fn advance_by(&mut self, duration: Duration) -> Result<ClockSnapshot, ClockError> {
        let now = self
            .monotonic_anchor
            .unwrap_or_else(Instant::now)
            .checked_add(duration)
            .ok_or(ClockError::Time(RationalError::Overflow))?;
        self.tick_at(now)
    }

    pub fn play(&mut self) -> Result<ClockSnapshot, ClockError> {
        self.play_at(Instant::now())
    }

    /// Start playback.  Playing after EOF deliberately restarts at the
    /// selected range beginning, matching the editor transport contract.
    pub fn play_at(&mut self, now: Instant) -> Result<ClockSnapshot, ClockError> {
        if self.bounds.end == self.bounds.start || self.duration == Time::ZERO {
            self.position = self.bounds.start;
            self.state = PlaybackState::Ended;
            self.eof = true;
            self.monotonic_anchor = None;
            return Ok(self.snapshot());
        }
        if self.eof || self.position >= self.bounds.end {
            self.position = self.bounds.start;
            self.eof = false;
        }
        if self.position < self.bounds.start {
            self.position = self.bounds.start;
        }
        self.state = PlaybackState::Playing;
        self.position_anchor = self.position;
        self.monotonic_anchor = Some(now);
        if let Some(audio) = &mut self.audio {
            audio.anchor_consumed = audio.last_consumed;
        }
        Ok(self.snapshot())
    }

    pub fn pause(&mut self) -> Result<ClockSnapshot, ClockError> {
        self.pause_at(Instant::now())
    }

    pub fn pause_at(&mut self, now: Instant) -> Result<ClockSnapshot, ClockError> {
        self.tick_at(now)?;
        if self.state == PlaybackState::Playing {
            self.state = PlaybackState::Paused;
            self.monotonic_anchor = None;
            self.position_anchor = self.position;
        }
        Ok(self.snapshot())
    }

    /// Seek to a project time, clamping to the selected range.  Seeking to the
    /// exclusive end is a valid terminal/UI position and leaves the clock
    /// paused at EOF; the decoder can choose the final decodable frame before
    /// that boundary separately.
    pub fn seek(&mut self, time: Time) -> Result<ClockSnapshot, ClockError> {
        self.seek_at(time, Instant::now())
    }

    pub fn seek_at(&mut self, time: Time, now: Instant) -> Result<ClockSnapshot, ClockError> {
        self.tick_at(now)?;
        self.generation = next_generation(self.generation);
        let was_playing = self.state == PlaybackState::Playing;
        self.position = clamp_time(time, self.bounds.start, self.bounds.end);
        self.eof = self.position == self.bounds.end;
        self.state = if self.eof {
            PlaybackState::Ended
        } else if was_playing {
            PlaybackState::Playing
        } else {
            PlaybackState::Paused
        };
        self.position_anchor = self.position;
        self.monotonic_anchor = if was_playing && !self.eof {
            Some(now)
        } else {
            None
        };
        if let Some(audio) = &mut self.audio {
            audio.anchor_consumed = audio.last_consumed;
        }
        Ok(self.snapshot())
    }

    pub fn set_range(&mut self, range: Option<TimeRange>) -> Result<ClockSnapshot, ClockError> {
        self.set_range_at(range, Instant::now())
    }

    pub fn set_range_at(
        &mut self,
        range: Option<TimeRange>,
        now: Instant,
    ) -> Result<ClockSnapshot, ClockError> {
        self.tick_at(now)?;
        let bounds = match range {
            Some(range) => {
                if range.start < Time::ZERO || range.end > self.duration {
                    return Err(ClockError::RangeOutsideDuration);
                }
                ClockBounds {
                    start: range.start,
                    end: range.end,
                }
            }
            None => ClockBounds {
                start: Time::ZERO,
                end: self.duration,
            },
        };
        if bounds.end <= bounds.start && self.duration > Time::ZERO {
            return Err(ClockError::InvalidRange);
        }
        self.generation = next_generation(self.generation);
        self.bounds = bounds;
        self.position = clamp_time(self.position, bounds.start, bounds.end);
        self.eof = self.position == bounds.end;
        if self.eof {
            self.state = PlaybackState::Ended;
            self.monotonic_anchor = None;
        } else {
            self.position_anchor = self.position;
            if self.state == PlaybackState::Playing {
                self.monotonic_anchor = Some(now);
            }
        }
        if let Some(audio) = &mut self.audio {
            audio.anchor_consumed = audio.last_consumed;
        }
        Ok(self.snapshot())
    }

    /// Configure the audio-consumption clock and switch to audio-driven mode.
    /// `consumed_samples` is an absolute device counter owned by the audio
    /// callback; the callback itself should only increment/report it.
    pub fn configure_audio(
        &mut self,
        sample_rate: u32,
        latency_samples: u64,
        consumed_samples: u64,
    ) -> Result<ClockSnapshot, ClockError> {
        self.configure_audio_at(
            sample_rate,
            latency_samples,
            consumed_samples,
            Instant::now(),
        )
    }

    pub fn configure_audio_at(
        &mut self,
        sample_rate: u32,
        latency_samples: u64,
        consumed_samples: u64,
        now: Instant,
    ) -> Result<ClockSnapshot, ClockError> {
        if sample_rate == 0 {
            return Err(ClockError::InvalidAudioRate);
        }
        self.tick_at(now)?;
        self.mode = ClockMode::AudioDriven;
        self.audio = Some(AudioClock {
            sample_rate,
            latency_samples,
            anchor_consumed: consumed_samples,
            last_consumed: consumed_samples,
        });
        self.position_anchor = self.position;
        self.monotonic_anchor = None;
        Ok(self.snapshot())
    }

    /// Report an absolute consumed-sample count.  The configured output
    /// latency is subtracted before mapping samples to project time.
    pub fn on_audio_consumed(
        &mut self,
        consumed_samples: u64,
    ) -> Result<ClockSnapshot, ClockError> {
        if self.mode != ClockMode::AudioDriven {
            return Err(ClockError::AudioClockDisabled);
        }
        let Some(audio) = &mut self.audio else {
            return Err(ClockError::AudioNotConfigured);
        };
        if consumed_samples < audio.last_consumed {
            return Err(ClockError::AudioCounterRewound);
        }
        audio.last_consumed = consumed_samples;
        if self.state != PlaybackState::Playing {
            return Ok(self.snapshot());
        }
        let delta = consumed_samples.saturating_sub(audio.anchor_consumed);
        let audible = delta.saturating_sub(audio.latency_samples);
        let delta_time = samples_to_time(audible, audio.sample_rate)?;
        self.apply_position(self.position_anchor.checked_add(delta_time)?);
        Ok(self.snapshot())
    }

    /// Switch to monotonic playback after an audio-device loss.  The current
    /// position is preserved and a new generation invalidates queued audio.
    pub fn on_audio_device_lost(&mut self, now: Instant) -> Result<ClockSnapshot, ClockError> {
        if self.mode == ClockMode::Silent {
            return Ok(self.snapshot());
        }
        self.generation = next_generation(self.generation);
        self.mode = ClockMode::Silent;
        self.position_anchor = self.position;
        self.monotonic_anchor = (self.state == PlaybackState::Playing).then_some(now);
        Ok(self.snapshot())
    }

    /// Re-enable an audio-consumption clock at the current project position.
    pub fn use_audio_clock(&mut self, now: Instant) -> Result<ClockSnapshot, ClockError> {
        let Some(audio) = &mut self.audio else {
            return Err(ClockError::AudioNotConfigured);
        };
        self.generation = next_generation(self.generation);
        self.mode = ClockMode::AudioDriven;
        audio.anchor_consumed = audio.last_consumed;
        self.position_anchor = self.position;
        self.monotonic_anchor = None;
        if self.state == PlaybackState::Playing && self.eof {
            self.state = PlaybackState::Ended;
        }
        let _ = now;
        Ok(self.snapshot())
    }

    fn apply_position(&mut self, candidate: Time) {
        if candidate >= self.bounds.end {
            self.position = self.bounds.end;
            self.state = PlaybackState::Ended;
            self.eof = true;
            self.monotonic_anchor = None;
        } else if candidate <= self.bounds.start {
            self.position = self.bounds.start;
            self.eof = false;
        } else {
            self.position = candidate;
            self.eof = false;
        }
    }
}

fn next_generation(current: Generation) -> Generation {
    current.checked_add(1).unwrap_or(1)
}

fn duration_to_time(duration: Duration) -> Result<Time, ClockError> {
    let nanos = i64::try_from(duration.as_nanos())
        .map_err(|_| ClockError::Time(RationalError::Overflow))?;
    Ok(Time::new(nanos, 1_000_000_000)?)
}

fn samples_to_time(samples: u64, sample_rate: u32) -> Result<Time, ClockError> {
    let samples = i64::try_from(samples).map_err(|_| ClockError::Time(RationalError::Overflow))?;
    Ok(Time::new(samples, sample_rate)?)
}

fn clamp_time(value: Time, low: Time, high: Time) -> Time {
    value.max(low).min(high)
}
