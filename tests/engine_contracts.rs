//! Path-based P5 contract tests.
//!
//! The modules are included by path so the contracts can be tested in
//! isolation as well as through the public engine module.

#![allow(dead_code, unused_imports)]

mod project {
    pub use slicer::project::*;
}

#[path = "../src/engine/api.rs"]
mod api;
#[path = "../src/engine/audio.rs"]
mod audio;
#[path = "../src/engine/cache.rs"]
mod cache;
#[path = "../src/engine/clock.rs"]
mod clock;
#[path = "../src/engine/legacy.rs"]
mod legacy;
#[path = "../src/engine/scheduler.rs"]
mod scheduler;

use api::{
    FrameCacheKey, FrameLease, FramePriority, FrameRequest, PlaybackCommand, PlaybackCommandKind,
    PlaybackEvent, PlaybackEventKind, PlaybackState, QueueError, RevisionTag, SourceId, WorkTag,
    command_channel, event_channel,
};
use audio::{AudioInterval, AudioMixConfig, AudioMixer, ClippingPolicy};
use cache::{CacheInsertStatus, FrameCache};
use clock::{ClockMode, ProjectClock};
use scheduler::{PreviewScheduler, VisibleClip};
use slicer::project::{AssetId, ClipId, FrameRate, ProjectId, Time, TimeRange, TrackId};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn revision() -> RevisionTag {
    RevisionTag::new(ProjectId::new(7), 3)
}

fn tag(generation: u64) -> WorkTag {
    WorkTag::new(revision(), generation)
}

fn range(start: i64, end: i64) -> TimeRange {
    TimeRange::new(Time::from_integer(start), Time::from_integer(end)).expect("valid range")
}

fn visible_clip(project_time: Time) -> VisibleClip {
    VisibleClip::new(
        ClipId::new(11),
        TrackId::new(12),
        AssetId::new(13),
        SourceId::new(14),
        range(0, 10),
        range(0, 10),
        project_time,
        project_time,
    )
    .expect("visible clip")
}

fn frame(key: FrameCacheKey, work_tag: WorkTag, value: u8) -> FrameLease {
    FrameLease::from_rgba(
        key,
        work_tag,
        key.time,
        Some(Time::from_integer(1)),
        1,
        1,
        vec![value, 0, 0, 255],
    )
    .expect("valid frame")
}

#[test]
fn clock_pause_seek_range_and_eof_are_exact() {
    let now = Instant::now();
    let mut clock = ProjectClock::new_at(
        Time::from_integer(10),
        Some(range(2, 5)),
        ClockMode::Silent,
        now,
    )
    .expect("clock");
    assert_eq!(clock.position(), Time::from_integer(2));
    assert_eq!(clock.state(), PlaybackState::Paused);

    clock.play_at(now).expect("play");
    clock
        .tick_at(now + Duration::from_millis(1_500))
        .expect("tick");
    assert_eq!(clock.position(), Time::new(7, 2).unwrap());

    clock.pause_at(now + Duration::from_secs(2)).expect("pause");
    assert_eq!(clock.position(), Time::from_integer(4));
    clock
        .tick_at(now + Duration::from_secs(9))
        .expect("paused tick");
    assert_eq!(clock.position(), Time::from_integer(4));

    clock
        .seek_at(Time::from_integer(-1), now + Duration::from_secs(9))
        .expect("clamped seek");
    assert_eq!(clock.position(), Time::from_integer(2));
    assert_eq!(clock.generation(), 1);
    clock.seek_at(Time::from_integer(5), now).expect("end seek");
    assert_eq!(clock.position(), Time::from_integer(5));
    assert_eq!(clock.state(), PlaybackState::Ended);
    assert!(clock.eof());

    clock.play_at(now).expect("replay");
    assert_eq!(clock.position(), Time::from_integer(2));
    assert_eq!(clock.state(), PlaybackState::Playing);
    clock
        .tick_at(now + Duration::from_secs(3))
        .expect("range eof");
    assert_eq!(clock.position(), Time::from_integer(5));
    assert_eq!(clock.state(), PlaybackState::Ended);
    assert!(clock.eof());
    clock.play_at(now).expect("range replay");
    assert_eq!(clock.position(), Time::from_integer(2));
}

#[test]
fn audio_clock_uses_consumption_and_latency_not_wall_time() {
    let now = Instant::now();
    let mut clock = ProjectClock::new_at(Time::from_integer(2), None, ClockMode::AudioDriven, now)
        .expect("clock");
    clock
        .configure_audio_at(48_000, 480, 0, now)
        .expect("audio config");
    clock.play_at(now).expect("play");
    clock
        .tick_at(now + Duration::from_secs(1))
        .expect("wall tick");
    assert_eq!(clock.position(), Time::ZERO);
    clock.on_audio_consumed(480).expect("latency");
    assert_eq!(clock.position(), Time::ZERO);
    clock.on_audio_consumed(48_480).expect("one second");
    assert_eq!(clock.position(), Time::from_integer(1));
    clock.on_audio_consumed(96_480).expect("eof");
    assert_eq!(clock.position(), Time::from_integer(2));
    assert_eq!(clock.state(), PlaybackState::Ended);
    clock.play_at(now).expect("replay");
    assert_eq!(clock.position(), Time::ZERO);
}

#[test]
fn scheduler_is_bounded_and_rejects_stale_generations() {
    let mut scheduler = PreviewScheduler::new(2, 3).expect("scheduler");
    let current = tag(0);
    let clip = visible_clip(Time::from_integer(5));
    scheduler
        .schedule_visible(
            current,
            Time::from_integer(5),
            Time::from_integer(1),
            [clip],
        )
        .expect("schedule");
    assert!(scheduler.pending_len() <= 2);
    let exact = scheduler.poll_next().expect("exact request");
    assert_eq!(exact.priority, FramePriority::Exact);
    assert_eq!(exact.tag, current);

    let newer = tag(1);
    scheduler.set_tag(newer);
    assert_eq!(scheduler.pending_len(), 0);
    assert_eq!(
        scheduler.enqueue(exact.clone()),
        Err(scheduler::SchedulerError::StaleGeneration)
    );
    assert_eq!(
        scheduler.accept_request(&exact),
        Err(scheduler::SchedulerError::StaleGeneration)
    );

    scheduler
        .schedule_visible(
            newer,
            Time::from_integer(5),
            Time::from_integer(1),
            [visible_clip(Time::from_integer(5))],
        )
        .expect("new schedule");
    assert_eq!(scheduler.poll_next().expect("new exact").tag, newer);
}

#[test]
fn frame_cache_evicts_lru_entries_within_both_bounds() {
    let mut cache = FrameCache::new(2, 8);
    let key_one = FrameCacheKey::new(AssetId::new(1), SourceId::new(1), 3, Time::ZERO);
    let key_two = FrameCacheKey::new(AssetId::new(2), SourceId::new(1), 3, Time::ZERO);
    let key_three = FrameCacheKey::new(AssetId::new(3), SourceId::new(1), 3, Time::ZERO);
    assert_eq!(
        cache.insert(frame(key_one, tag(0), 1)).status,
        CacheInsertStatus::Inserted
    );
    assert_eq!(
        cache.insert(frame(key_two, tag(0), 2)).status,
        CacheInsertStatus::Inserted
    );
    assert_eq!(cache.get(key_one, tag(1)).expect("cache hit").tag, tag(1));
    let outcome = cache.insert(frame(key_three, tag(1), 3));
    assert_eq!(outcome.evicted_entries, 1);
    assert!(cache.contains(key_one));
    assert!(!cache.contains(key_two));
    assert!(cache.contains(key_three));
    assert_eq!(cache.len(), 2);
    assert_eq!(cache.bytes(), 8);

    let mut small = FrameCache::new(2, 2);
    let rejected = small.insert(frame(key_one, tag(0), 4));
    assert_eq!(rejected.status, CacheInsertStatus::RejectedOversize);
    assert!(small.is_empty());
}

#[test]
fn audio_mix_sums_clips_and_keeps_monitor_mute_separate() {
    let interval_one = AudioInterval::new(
        ClipId::new(1),
        TrackId::new(1),
        AssetId::new(1),
        SourceId::new(1),
        range(0, 1),
        range(0, 1),
        48_000,
        1,
        vec![0.75],
        1.0,
        false,
    )
    .expect("first interval");
    let interval_two = AudioInterval::new(
        ClipId::new(2),
        TrackId::new(2),
        AssetId::new(2),
        SourceId::new(2),
        range(0, 1),
        range(0, 1),
        48_000,
        1,
        vec![0.75],
        1.0,
        false,
    )
    .expect("second interval");
    let mixer = AudioMixer::new(
        AudioMixConfig::new(48_000, 1, ClippingPolicy::HardClip).expect("mix config"),
    );
    let mixed = mixer
        .mix_frames(&[interval_one.clone(), interval_two.clone()], Time::ZERO, 1)
        .expect("mix");
    assert_eq!(mixed.samples, vec![1.0]);
    assert_eq!(mixed.clipped_samples, 1);
    assert_eq!(mixed.export_samples(), &[1.0]);
    assert_eq!(mixed.monitor_samples(), vec![1.0]);

    let muted = mixer
        .with_monitor_mute(true)
        .mix_frames(&[interval_one, interval_two], Time::ZERO, 1)
        .expect("muted monitor mix");
    assert_eq!(muted.export_samples(), &[1.0]);
    assert_eq!(muted.monitor_samples(), vec![0.0]);
}

#[test]
fn command_and_event_channels_are_owned_and_non_blocking() {
    let (commands, receiver) = command_channel(1).expect("command channel");
    let command = PlaybackCommand::new(tag(0), PlaybackCommandKind::Play);
    commands.try_send(command.clone()).expect("first command");
    assert_eq!(commands.try_send(command), Err(QueueError::Full));
    assert_eq!(
        receiver.try_recv().expect("receive").unwrap().kind,
        PlaybackCommandKind::Play
    );
    assert_eq!(receiver.try_recv().expect("empty").is_none(), true);

    let (events, event_receiver) = event_channel(1).expect("event channel");
    events
        .try_send(PlaybackEvent::new(
            tag(0),
            PlaybackEventKind::CurrentTime {
                time: Time::ZERO,
                state: PlaybackState::Paused,
                eof: false,
            },
        ))
        .expect("event");
    assert!(event_receiver.try_recv().expect("event receive").is_some());
}

#[test]
fn commands_are_serializable_owned_values() {
    let command = PlaybackCommand::new(
        tag(4),
        PlaybackCommandKind::Load {
            path: PathBuf::from("media/example.mov"),
            duration: Time::from_integer(10),
            range: Some(range(2, 8)),
        },
    );
    let encoded = serde_json::to_string(&command).expect("serialize command");
    let decoded: PlaybackCommand = serde_json::from_str(&encoded).expect("deserialize command");
    assert_eq!(decoded, command);
    assert!(command.is_stale_for(tag(5)));
    assert!(!command.is_stale_for(tag(4)));
}
