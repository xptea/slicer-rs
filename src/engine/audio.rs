//! Software audio interval mixing.
//!
//! This module owns no device and performs no I/O.  It turns immutable source
//! sample intervals into an output block for either preview or export.  Clip
//! gain/mute is applied to the mix; monitor mute is applied only when a caller
//! copies that mix to a preview device, so export samples remain unchanged.

use crate::project::{AssetId, ClipId, RationalError, Time, TimeRange, TrackId};
use std::error::Error;
use std::fmt;
use std::sync::Arc;

use super::api::SourceId;

/// A decoded, source-time-aligned audio interval.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioInterval {
    pub clip_id: ClipId,
    pub track_id: TrackId,
    pub asset_id: AssetId,
    pub source_id: SourceId,
    pub project_range: TimeRange,
    pub source_range: TimeRange,
    pub sample_rate: u32,
    pub channels: u16,
    /// Interleaved samples beginning at `source_range.start`.
    pub samples: Arc<[f32]>,
    pub gain: f64,
    pub muted: bool,
    pub embedded: bool,
}

impl AudioInterval {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        clip_id: ClipId,
        track_id: TrackId,
        asset_id: AssetId,
        source_id: SourceId,
        project_range: TimeRange,
        source_range: TimeRange,
        sample_rate: u32,
        channels: u16,
        samples: Vec<f32>,
        gain: f64,
        muted: bool,
    ) -> Result<Self, AudioMixError> {
        let interval = Self {
            clip_id,
            track_id,
            asset_id,
            source_id,
            project_range,
            source_range,
            sample_rate,
            channels,
            samples: Arc::from(samples),
            gain,
            muted,
            embedded: false,
        };
        interval.validate()?;
        Ok(interval)
    }

    pub fn with_embedded(mut self, embedded: bool) -> Self {
        self.embedded = embedded;
        self
    }

    pub fn validate(&self) -> Result<(), AudioMixError> {
        if self.sample_rate == 0 || self.channels == 0 {
            return Err(AudioMixError::InvalidLayout);
        }
        if self.project_range.start < Time::ZERO || self.source_range.start < Time::ZERO {
            return Err(AudioMixError::NegativeRange);
        }
        if self.gain.is_nan() || !self.gain.is_finite() || self.gain < 0.0 {
            return Err(AudioMixError::InvalidGain);
        }
        if !self
            .samples
            .len()
            .is_multiple_of(usize::from(self.channels))
        {
            return Err(AudioMixError::SampleLayoutMismatch);
        }
        if self.samples.iter().any(|sample| !sample.is_finite()) {
            return Err(AudioMixError::NonFiniteSample);
        }
        Ok(())
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels)
    }
}

/// Policy shared by preview and export paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClippingPolicy {
    /// Saturate to the normalized PCM range and count clipped samples.
    HardClip,
    /// Apply `tanh` above the normalized range while retaining a bounded
    /// signal.  This is deterministic but intentionally not a limiter.
    SoftClip,
    /// Fail the block if any mixed sample leaves the normalized range.
    Reject,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioMixConfig {
    pub output_sample_rate: u32,
    pub output_channels: u16,
    pub clipping: ClippingPolicy,
    /// Preview-only flag.  It is never consulted while producing `samples`.
    pub monitor_muted: bool,
}

impl AudioMixConfig {
    pub fn new(
        output_sample_rate: u32,
        output_channels: u16,
        clipping: ClippingPolicy,
    ) -> Result<Self, AudioMixError> {
        if output_sample_rate == 0 || output_channels == 0 {
            return Err(AudioMixError::InvalidLayout);
        }
        Ok(Self {
            output_sample_rate,
            output_channels,
            clipping,
            monitor_muted: false,
        })
    }
}

impl Default for AudioMixConfig {
    fn default() -> Self {
        Self {
            output_sample_rate: 48_000,
            output_channels: 2,
            clipping: ClippingPolicy::HardClip,
            monitor_muted: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MixedAudio {
    pub start: Time,
    pub sample_rate: u32,
    pub channels: u16,
    /// The unmuted mix.  This is the buffer used by export.
    pub samples: Vec<f32>,
    pub clipped_samples: u64,
    pub monitor_muted: bool,
}

impl MixedAudio {
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels)
    }

    pub fn export_samples(&self) -> &[f32] {
        &self.samples
    }

    /// Return the preview-device buffer.  This operation happens outside an
    /// audio callback; callback code should use [`Self::copy_monitor_into`].
    pub fn monitor_samples(&self) -> Vec<f32> {
        if self.monitor_muted {
            vec![0.0; self.samples.len()]
        } else {
            self.samples.clone()
        }
    }

    /// Copy monitor output without allocating.  A device callback can use
    /// this with a preallocated output slice after the block is prepared.
    pub fn copy_monitor_into(&self, output: &mut [f32]) -> Result<(), AudioMixError> {
        if output.len() != self.samples.len() {
            return Err(AudioMixError::OutputLengthMismatch {
                actual: output.len(),
                expected: self.samples.len(),
            });
        }
        if self.monitor_muted {
            output.fill(0.0);
        } else {
            output.copy_from_slice(&self.samples);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AudioMixError {
    InvalidLayout,
    InvalidGain,
    NegativeRange,
    SampleLayoutMismatch,
    NonFiniteSample,
    InvalidOutputRange,
    Time(RationalError),
    TooManyFrames,
    Clipping { value: f64 },
    OutputLengthMismatch { actual: usize, expected: usize },
}

impl fmt::Display for AudioMixError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLayout => formatter.write_str("audio sample layout must be positive"),
            Self::InvalidGain => formatter.write_str("audio gain must be finite and non-negative"),
            Self::NegativeRange => formatter.write_str("audio ranges must be non-negative"),
            Self::SampleLayoutMismatch => {
                formatter.write_str("audio sample count is not divisible by channel count")
            }
            Self::NonFiniteSample => formatter.write_str("audio samples must be finite"),
            Self::InvalidOutputRange => formatter.write_str("audio output range must be non-empty"),
            Self::Time(error) => error.fmt(formatter),
            Self::TooManyFrames => formatter.write_str("audio output frame count is too large"),
            Self::Clipping { value } => write!(formatter, "audio mix clipped at {value}"),
            Self::OutputLengthMismatch { actual, expected } => {
                write!(
                    formatter,
                    "audio output has {actual} samples; expected {expected}"
                )
            }
        }
    }
}

impl Error for AudioMixError {}

impl From<RationalError> for AudioMixError {
    fn from(error: RationalError) -> Self {
        Self::Time(error)
    }
}

/// A device-independent mixer.  It can be shared by preview and offline
/// export workers because it has no mutable clip or project state.
#[derive(Clone, Copy, Debug)]
pub struct AudioMixer {
    config: AudioMixConfig,
}

impl AudioMixer {
    pub const fn new(config: AudioMixConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> AudioMixConfig {
        self.config
    }

    pub fn with_monitor_mute(mut self, muted: bool) -> Self {
        self.config.monitor_muted = muted;
        self
    }

    pub fn set_monitor_mute(&mut self, muted: bool) {
        self.config.monitor_muted = muted;
    }

    /// Mix exactly `frames` output frames beginning at `start`.
    pub fn mix_frames(
        &self,
        intervals: &[AudioInterval],
        start: Time,
        frames: usize,
    ) -> Result<MixedAudio, AudioMixError> {
        if start < Time::ZERO {
            return Err(AudioMixError::NegativeRange);
        }
        for interval in intervals {
            interval.validate()?;
        }
        let sample_count = frames
            .checked_mul(usize::from(self.config.output_channels))
            .ok_or(AudioMixError::TooManyFrames)?;
        let mut samples = vec![0.0_f32; sample_count];
        let mut clipped_samples = 0_u64;
        for frame in 0..frames {
            let frame_offset = Time::new(
                i64::try_from(frame).map_err(|_| AudioMixError::TooManyFrames)?,
                self.config.output_sample_rate,
            )?;
            let project_time = start.checked_add(frame_offset)?;
            for interval in intervals {
                if interval.muted || !interval.project_range.contains(project_time) {
                    continue;
                }
                let source_position = source_position(interval, project_time)?;
                for channel in 0..usize::from(self.config.output_channels) {
                    let value =
                        sample_for_channel(interval, source_position, channel) * interval.gain;
                    let index = frame * usize::from(self.config.output_channels) + channel;
                    samples[index] += value as f32;
                }
            }
        }
        for sample in &mut samples {
            let value = f64::from(*sample);
            if value.abs() > 1.0 {
                clipped_samples = clipped_samples.saturating_add(1);
                *sample = match self.config.clipping {
                    ClippingPolicy::HardClip => value.clamp(-1.0, 1.0) as f32,
                    ClippingPolicy::SoftClip => value.tanh() as f32,
                    ClippingPolicy::Reject => {
                        return Err(AudioMixError::Clipping { value });
                    }
                };
            }
        }
        Ok(MixedAudio {
            start,
            sample_rate: self.config.output_sample_rate,
            channels: self.config.output_channels,
            samples,
            clipped_samples,
            monitor_muted: self.config.monitor_muted,
        })
    }

    /// Mix every output sample whose timestamp is in a half-open range.
    pub fn mix_range(
        &self,
        intervals: &[AudioInterval],
        range: TimeRange,
    ) -> Result<MixedAudio, AudioMixError> {
        let duration = range.duration()?;
        let exact_frames = duration.checked_mul_integer(self.config.output_sample_rate as i64)?;
        let frames =
            usize::try_from(exact_frames.ceil_i128()).map_err(|_| AudioMixError::TooManyFrames)?;
        if frames == 0 {
            return Err(AudioMixError::InvalidOutputRange);
        }
        self.mix_frames(intervals, range.start, frames)
    }
}

fn source_position(interval: &AudioInterval, project_time: Time) -> Result<f64, AudioMixError> {
    let project_elapsed = project_time.checked_sub(interval.project_range.start)?;
    let project_duration = interval.project_range.duration()?;
    let source_duration = interval.source_range.duration()?;
    let normalized = project_elapsed.checked_div(project_duration)?;
    let source_elapsed = source_duration.checked_mul(normalized)?;
    Ok(source_elapsed.to_f64() * f64::from(interval.sample_rate))
}

fn sample_for_channel(interval: &AudioInterval, position: f64, output_channel: usize) -> f64 {
    let input_channels = usize::from(interval.channels);
    let frames = interval.frames();
    if frames == 0 || !position.is_finite() || position < 0.0 || position >= frames as f64 {
        return 0.0;
    }
    let lower = position.floor() as usize;
    let upper = (lower + 1).min(frames.saturating_sub(1));
    let mix = position - lower as f64;
    let value_at = |frame: usize, channel: usize| -> f64 {
        f64::from(interval.samples[frame * input_channels + channel])
    };
    if input_channels == 1 {
        let lower = value_at(lower, 0);
        let upper = value_at(upper, 0);
        return lower + (upper - lower) * mix;
    }
    if output_channel < input_channels {
        let lower = value_at(lower, output_channel);
        let upper = value_at(upper, output_channel);
        return lower + (upper - lower) * mix;
    }
    // More output channels than source channels: repeat the last source
    // channel, a deterministic fallback that avoids introducing silence into
    // a layout expansion.
    let channel = input_channels - 1;
    let lower = value_at(lower, channel);
    let upper = value_at(upper, channel);
    lower + (upper - lower) * mix
}
