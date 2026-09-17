//! Bounded preview scheduling and visible-clip prefetch.
//!
//! Scheduling is deliberately a small worker-side data structure.  A new
//! revision or seek generation clears old work; requests are owned values and
//! `poll_next` never waits for a decoder.  Exact requests take precedence over
//! prefetch, and a full queue evicts prefetch before it evicts an exact seek.

use super::api::{
    FrameCacheKey, FramePriority, FrameRequest, Generation, RevisionTag, SourceId, WorkTag,
};
use crate::project::{AssetId, ClipId, Time, TimeRange, TrackId};
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt;

/// The visible portion of one source-backed clip at the current project time.
#[derive(Clone, Debug, PartialEq)]
pub struct VisibleClip {
    pub clip_id: ClipId,
    pub track_id: TrackId,
    pub asset_id: AssetId,
    pub source_id: SourceId,
    pub project_range: TimeRange,
    pub source_range: TimeRange,
    pub project_time: Time,
    pub source_time: Time,
}

impl VisibleClip {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        clip_id: ClipId,
        track_id: TrackId,
        asset_id: AssetId,
        source_id: SourceId,
        project_range: TimeRange,
        source_range: TimeRange,
        project_time: Time,
        source_time: Time,
    ) -> Result<Self, SchedulerError> {
        if project_time < project_range.start || project_time >= project_range.end {
            return Err(SchedulerError::TimeOutsideClip);
        }
        if source_time < source_range.start || source_time >= source_range.end {
            return Err(SchedulerError::SourceTimeOutsideClip);
        }
        Ok(Self {
            clip_id,
            track_id,
            asset_id,
            source_id,
            project_range,
            source_range,
            project_time,
            source_time,
        })
    }

    fn request_at(
        &self,
        tag: WorkTag,
        project_time: Time,
        source_time: Time,
        priority: FramePriority,
    ) -> Option<FrameRequest> {
        if !self.project_range.contains(project_time) || !self.source_range.contains(source_time) {
            return None;
        }
        Some(FrameRequest {
            tag,
            clip_id: self.clip_id,
            track_id: self.track_id,
            key: FrameCacheKey::new(
                self.asset_id,
                self.source_id,
                tag.revision.revision,
                source_time,
            ),
            project_time,
            source_time,
            priority,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchedulerError {
    ZeroCapacity,
    NonPositiveFrameStep,
    NoGeneration,
    StaleGeneration,
    NotVisible,
    TimeOutsideClip,
    SourceTimeOutsideClip,
    TimeArithmetic,
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCapacity => formatter.write_str("scheduler capacity must be positive"),
            Self::NonPositiveFrameStep => {
                formatter.write_str("scheduler frame step must be positive")
            }
            Self::NoGeneration => formatter.write_str("scheduler has no active work generation"),
            Self::StaleGeneration => {
                formatter.write_str("scheduler rejected stale work generation")
            }
            Self::NotVisible => formatter.write_str("scheduler rejected a non-visible clip"),
            Self::TimeOutsideClip => {
                formatter.write_str("project time is outside the visible clip")
            }
            Self::SourceTimeOutsideClip => {
                formatter.write_str("source time is outside the visible clip")
            }
            Self::TimeArithmetic => formatter.write_str("scheduler time arithmetic overflowed"),
        }
    }
}

impl Error for SchedulerError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnqueueOutcome {
    pub queued: bool,
    pub replaced: bool,
    pub evicted: bool,
}

#[derive(Clone, Debug)]
struct QueuedRequest {
    request: FrameRequest,
    sequence: u64,
}

/// A bounded latest-generation scheduler for preview frames.
pub struct PreviewScheduler {
    capacity: usize,
    prefetch_frames: usize,
    active_tag: Option<WorkTag>,
    visible: BTreeMap<ClipId, VisibleClip>,
    pending: VecDeque<QueuedRequest>,
    sequence: u64,
}

impl PreviewScheduler {
    pub fn new(capacity: usize, prefetch_frames: usize) -> Result<Self, SchedulerError> {
        if capacity == 0 {
            return Err(SchedulerError::ZeroCapacity);
        }
        Ok(Self {
            capacity,
            prefetch_frames,
            active_tag: None,
            visible: BTreeMap::new(),
            pending: VecDeque::new(),
            sequence: 0,
        })
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn prefetch_frames(&self) -> usize {
        self.prefetch_frames
    }

    pub fn active_tag(&self) -> Option<WorkTag> {
        self.active_tag
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Change the active revision/generation and invalidate all queued work.
    pub fn set_tag(&mut self, tag: WorkTag) {
        if self.active_tag != Some(tag) {
            self.pending.clear();
        }
        self.active_tag = Some(tag);
    }

    /// Advance the seek generation for a revision and clear old requests.
    pub fn set_generation(&mut self, revision: RevisionTag, generation: Generation) -> WorkTag {
        let tag = WorkTag::new(revision, generation);
        self.set_tag(tag);
        tag
    }

    /// Replace the visible set and queue the current frame plus bounded
    /// neighboring frames.  The source mapping is initially one-to-one, as in
    /// the P1 model; a future speed-ramp mapper can construct requests directly
    /// with [`Self::enqueue`].
    pub fn schedule_visible(
        &mut self,
        tag: WorkTag,
        project_time: Time,
        frame_step: Time,
        clips: impl IntoIterator<Item = VisibleClip>,
    ) -> Result<usize, SchedulerError> {
        if frame_step <= Time::ZERO {
            return Err(SchedulerError::NonPositiveFrameStep);
        }
        self.set_tag(tag);
        self.visible.clear();
        self.visible.extend(
            clips
                .into_iter()
                .filter(|clip| clip.project_range.contains(project_time))
                .map(|clip| (clip.clip_id, clip)),
        );
        self.pending.clear();

        let clips = self.visible.values().cloned().collect::<Vec<_>>();
        for clip in clips {
            if let Some(request) =
                clip.request_at(tag, project_time, clip.source_time, FramePriority::Exact)
            {
                let _ = self.enqueue(request)?;
            }
            for offset in 1..=self.prefetch_frames {
                let offset = i64::try_from(offset).map_err(|_| SchedulerError::TimeArithmetic)?;
                let delta = frame_step
                    .checked_mul_integer(offset)
                    .map_err(|_| SchedulerError::TimeArithmetic)?;
                let targets = [
                    (
                        project_time.checked_add(delta),
                        clip.source_time.checked_add(delta),
                    ),
                    (
                        project_time.checked_sub(delta),
                        clip.source_time.checked_sub(delta),
                    ),
                ];
                for (project_target, source_target) in targets {
                    let (Ok(project_target), Ok(source_target)) = (project_target, source_target)
                    else {
                        continue;
                    };
                    if let Some(request) =
                        clip.request_at(tag, project_target, source_target, FramePriority::Prefetch)
                    {
                        let _ = self.enqueue(request)?;
                    }
                }
            }
        }
        Ok(self.pending.len())
    }

    /// Queue one request if it belongs to the current generation and a visible
    /// clip.  Repeated requests for the same clip/time/priority replace the
    /// old value instead of growing the queue.
    pub fn enqueue(&mut self, request: FrameRequest) -> Result<EnqueueOutcome, SchedulerError> {
        let Some(active_tag) = self.active_tag else {
            return Err(SchedulerError::NoGeneration);
        };
        if request.tag != active_tag {
            return Err(SchedulerError::StaleGeneration);
        }
        if !self.visible.contains_key(&request.clip_id) {
            return Err(SchedulerError::NotVisible);
        }

        let replaced = self.pending.iter().position(|queued| {
            queued.request.clip_id == request.clip_id
                && queued.request.priority == request.priority
                && queued.request.key == request.key
        });
        if let Some(index) = replaced {
            self.pending.remove(index);
        }

        let mut evicted = false;
        if self.pending.len() >= self.capacity {
            let index = self
                .pending
                .iter()
                .position(|queued| queued.request.priority == FramePriority::Prefetch)
                .unwrap_or(0);
            self.pending.remove(index);
            evicted = true;
        }
        self.sequence = self.sequence.wrapping_add(1);
        self.pending.push_back(QueuedRequest {
            request,
            sequence: self.sequence,
        });
        Ok(EnqueueOutcome {
            queued: true,
            replaced: replaced.is_some(),
            evicted,
        })
    }

    /// Return the next exact request, or the oldest prefetch, without waiting.
    pub fn poll_next(&mut self) -> Option<FrameRequest> {
        let index = self
            .pending
            .iter()
            .enumerate()
            .min_by_key(|(_, queued)| {
                (
                    if queued.request.priority == FramePriority::Exact {
                        0_u8
                    } else {
                        1_u8
                    },
                    queued.sequence,
                )
            })
            .map(|(index, _)| index)?;
        self.pending.remove(index).map(|queued| queued.request)
    }

    /// Validate a source result before it is allowed to reach the compositor.
    pub fn accept_request(&self, request: &FrameRequest) -> Result<(), SchedulerError> {
        let Some(active_tag) = self.active_tag else {
            return Err(SchedulerError::NoGeneration);
        };
        if request.tag != active_tag {
            return Err(SchedulerError::StaleGeneration);
        }
        if !self.visible.contains_key(&request.clip_id) {
            return Err(SchedulerError::NotVisible);
        }
        Ok(())
    }
}
