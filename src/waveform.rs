//! Bounded, cancellable audio waveform extraction.
//!
//! The editor only needs a small set of peak values, not decoded audio.  A
//! worker asks the bundled FFmpeg for mono 16-bit PCM in a streamed WAV
//! container, reduces samples into a fixed number of min/max bins, and sends
//! the completed result back to the GPUI thread.  No decoded audio is kept in
//! memory and a newer request cancels the current FFmpeg process.

use crate::media::Binaries;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

/// The sample rate requested from FFmpeg.  It is high enough to retain
/// speech transients while keeping extraction and the peak reducer cheap.
const SAMPLE_RATE_TEXT: &str = "8000";
const MIN_BINS: usize = 512;
const MAX_BINS: usize = 4_096;
const MAX_WAV_CHUNK: u32 = 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;
const EXTRACTION_TIMEOUT: Duration = Duration::from_secs(15);

/// One full-waveform bin. Values are normalized to [-1, 1].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Peak {
    pub min: f32,
    pub max: f32,
}

impl Peak {
    fn silence() -> Self {
        Self { min: 0.0, max: 0.0 }
    }
}

/// A full-duration, resolution-independent representation of an audio track.
#[derive(Clone, Debug, PartialEq)]
pub struct Waveform {
    duration: f64,
    peaks: Arc<[Peak]>,
    has_audio: bool,
}

impl Waveform {
    /// Build a waveform from already reduced peaks. This is also useful for
    /// deterministic tests and for callers that have their own decoder.
    pub fn from_peaks(duration: f64, peaks: Vec<Peak>, has_audio: bool) -> Self {
        let duration = if duration.is_finite() {
            duration.max(0.0)
        } else {
            0.0
        };
        let peaks = if peaks.is_empty() {
            vec![Peak::silence(); bins_for_duration(duration)]
        } else {
            peaks
                .into_iter()
                .map(|peak| Peak {
                    min: peak.min.clamp(-1.0, 1.0),
                    max: peak.max.clamp(-1.0, 1.0),
                })
                .collect()
        };
        Self {
            duration,
            peaks: peaks.into(),
            has_audio,
        }
    }

    /// A flat waveform used for silent audio and media without an audio track.
    pub fn silence(duration: f64) -> Self {
        let duration = if duration.is_finite() {
            duration.max(0.0)
        } else {
            0.0
        };
        Self {
            duration,
            peaks: vec![Peak::silence(); bins_for_duration(duration)].into(),
            has_audio: false,
        }
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn peaks(&self) -> &[Peak] {
        &self.peaks
    }

    /// Whether the source contained an audio stream. Silent audio still has
    /// this set to true; callers can use it when deciding whether to label a
    /// missing track.
    pub fn has_audio(&self) -> bool {
        self.has_audio
    }

    /// Merge peaks in a source-time interval, including trimmed clip offsets.
    pub fn peak_between(&self, start: f64, end: f64) -> Peak {
        if !start.is_finite()
            || !end.is_finite()
            || self.duration <= 0.0
            || end <= start
            || end <= 0.0
            || start >= self.duration
        {
            return Peak::silence();
        }
        let first = ((start.max(0.0) / self.duration * self.peaks.len() as f64).floor() as usize)
            .min(self.peaks.len());
        let last = ((end.min(self.duration) / self.duration * self.peaks.len() as f64).ceil()
            as usize)
            .min(self.peaks.len());
        self.peaks[first..last]
            .iter()
            .fold(Peak::silence(), |peak, source| Peak {
                min: peak.min.min(source.min),
                max: peak.max.max(source.max),
            })
    }

    /// Merge the source bins that cover one rendered timeline column.
    pub fn peak_for_column(&self, column: usize, columns: usize) -> Peak {
        if self.peaks.is_empty() || columns == 0 {
            return Peak::silence();
        }
        let first = column
            .saturating_mul(self.peaks.len())
            .checked_div(columns)
            .unwrap_or(0)
            .min(self.peaks.len() - 1);
        let last = ((column.saturating_add(1)).saturating_mul(self.peaks.len()) / columns)
            .max(first + 1)
            .min(self.peaks.len());
        let mut peak = Peak {
            min: 1.0,
            max: -1.0,
        };
        for source in &self.peaks[first..last] {
            peak.min = peak.min.min(source.min);
            peak.max = peak.max.max(source.max);
        }
        peak
    }
}

fn bins_for_duration(duration: f64) -> usize {
    if duration > 0.0 && duration.is_finite() {
        (duration.mul_add(120.0, 0.999).ceil() as usize).clamp(MIN_BINS, MAX_BINS)
    } else {
        MIN_BINS
    }
}

struct WaveformRequest {
    generation: u64,
    path: PathBuf,
    duration: f64,
    has_audio: bool,
}

/// A completed or failed background extraction result.
pub struct WaveformEvent {
    pub generation: u64,
    pub path: PathBuf,
    pub result: Result<Waveform, String>,
}

/// Latest-request-wins waveform extraction worker.
pub struct WaveformWorker {
    requests: Sender<WaveformRequest>,
    pub events: Receiver<WaveformEvent>,
    generation: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl WaveformWorker {
    pub fn new(binaries: Binaries) -> Self {
        let (requests, rx) = mpsc::channel::<WaveformRequest>();
        let (tx, events) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_generation = generation.clone();
        let worker_stop = stop.clone();
        thread::spawn(move || {
            while let Ok(mut request) = rx.recv() {
                // Do not let a slow decode make a queue of obsolete files.
                while let Ok(newer) = rx.try_recv() {
                    request = newer;
                }
                if worker_stop.load(Ordering::Acquire)
                    || worker_generation.load(Ordering::Acquire) != request.generation
                {
                    continue;
                }

                let result = match if request.has_audio {
                    extract(
                        &binaries,
                        &request.path,
                        request.duration,
                        request.generation,
                        &worker_generation,
                        &worker_stop,
                    )
                } else {
                    Ok(Some(Waveform::silence(request.duration)))
                } {
                    Ok(Some(waveform)) => Ok(waveform),
                    Ok(None) => continue,
                    Err(error) => Err(error),
                };
                if worker_stop.load(Ordering::Acquire)
                    || worker_generation.load(Ordering::Acquire) != request.generation
                {
                    continue;
                }
                if tx
                    .send(WaveformEvent {
                        generation: request.generation,
                        path: request.path,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            requests,
            events,
            generation,
            stop,
        }
    }

    /// Request the waveform for a new editor file. The returned generation is
    /// stable and can be used to reject late events for a previous file.
    pub fn request(&self, path: PathBuf, duration: f64, has_audio: bool) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.requests.send(WaveformRequest {
            generation,
            path,
            duration,
            has_audio,
        });
        generation
    }

    /// Cancel a current decode and discard queued requests.
    pub fn cancel(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }
}

impl Drop for WaveformWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        // Dropping the sender after this method returns closes the worker's
        // receive loop. If an FFmpeg process is active, generation/stop makes
        // the worker kill it before joining its pipe readers.
    }
}

fn is_cancelled(generation: u64, current_generation: &AtomicU64, stop: &AtomicBool) -> bool {
    stop.load(Ordering::Acquire) || current_generation.load(Ordering::Acquire) != generation
}

/// Run the bundled FFmpeg and reduce its streamed PCM output. `None` means a
/// newer request cancelled this extraction and must not be surfaced as an UI
/// error.
fn extract(
    binaries: &Binaries,
    path: &Path,
    duration: f64,
    generation: u64,
    current_generation: &Arc<AtomicU64>,
    stop: &Arc<AtomicBool>,
) -> Result<Option<Waveform>, String> {
    let mut child = Command::new(&binaries.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(path)
        .args([
            "-map",
            "0:a:0?",
            "-vn",
            "-sn",
            "-dn",
            "-ac",
            "1",
            "-ar",
            SAMPLE_RATE_TEXT,
            "-af",
            "aresample=async=1:first_pts=0",
            "-c:a",
            "pcm_s16le",
            "-f",
            "wav",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot start waveform extraction: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Waveform extractor did not expose stdout".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Waveform extractor did not expose stderr".to_owned())?;
    let reader_generation = Arc::clone(current_generation);
    let reader_stop = Arc::clone(stop);
    let reader = thread::spawn(move || {
        read_waveform(
            stdout,
            duration,
            generation,
            &reader_generation,
            &reader_stop,
        )
    });
    let error_reader = thread::spawn(move || read_bounded(stderr, MAX_ERROR_BYTES));

    let began = Instant::now();
    let status = loop {
        if is_cancelled(generation, current_generation, stop) {
            terminate(&mut child);
            let _ = reader.join();
            let _ = error_reader.join();
            return Ok(None);
        }
        if began.elapsed() >= EXTRACTION_TIMEOUT {
            terminate(&mut child);
            let _ = reader.join();
            let _ = error_reader.join();
            return Err("Waveform extraction timed out".to_owned());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                terminate(&mut child);
                let _ = reader.join();
                let _ = error_reader.join();
                return Err(format!("Cannot monitor waveform extraction: {error}"));
            }
        }
    };
    let waveform = reader
        .join()
        .map_err(|_| "Waveform decoder thread panicked".to_owned())?;
    let errors = error_reader
        .join()
        .map_err(|_| "Waveform error reader thread panicked".to_owned())?;
    if is_cancelled(generation, current_generation, stop) {
        return Ok(None);
    }
    if !status.success() {
        let details = String::from_utf8_lossy(&errors).trim().to_owned();
        return Err(if details.is_empty() {
            format!("Waveform extraction failed with {status}")
        } else {
            format!("Waveform extraction failed: {details}")
        });
    }
    waveform.map(Some)
}

fn terminate(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_bounded<R: Read>(mut reader: R, limit: usize) -> Vec<u8> {
    let mut output = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 4096];
    // Keep draining after the retained diagnostic prefix is full. Closing the
    // pipe early can make a chatty FFmpeg block forever on a full stderr pipe.
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let remaining = limit.saturating_sub(output.len());
                if remaining > 0 {
                    output.extend_from_slice(&buffer[..read.min(remaining)]);
                }
            }
        }
    }
    output
}

fn read_waveform<R: Read>(
    mut reader: R,
    duration_hint: f64,
    generation: u64,
    current_generation: &AtomicU64,
    stop: &AtomicBool,
) -> Result<Waveform, String> {
    let header = read_wav_header(&mut reader)?;
    if header.audio_format != 1 || header.bits_per_sample != 16 {
        return Err(format!(
            "Waveform extractor returned unsupported PCM format (format={}, bits={})",
            header.audio_format, header.bits_per_sample
        ));
    }
    if header.channels == 0 || header.channels > 8 {
        return Err("Waveform extractor returned an invalid channel count".to_owned());
    }
    let bytes_per_frame = usize::from(header.channels) * 2;
    if usize::from(header.block_align) != bytes_per_frame {
        return Err("Waveform extractor returned an invalid WAV block size".to_owned());
    }
    if header.sample_rate == 0 {
        return Err("Waveform extractor returned an invalid sample rate".to_owned());
    }

    let duration = if duration_hint.is_finite() && duration_hint > 0.0 {
        duration_hint
    } else {
        0.0
    };
    let bin_count = bins_for_duration(duration);
    let mut mins = vec![1.0_f32; bin_count];
    let mut maxs = vec![-1.0_f32; bin_count];
    let mut sample_index = 0_u64;
    let mut pending = Vec::with_capacity(bytes_per_frame * 4096);
    let mut remaining_data = header.data_size;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        if is_cancelled(generation, current_generation, stop) {
            return Err("cancelled".to_owned());
        }
        let read_limit = remaining_data.map_or(buffer.len(), |remaining| {
            remaining.min(buffer.len() as u64) as usize
        });
        if read_limit == 0 {
            break;
        }
        let read = reader
            .read(&mut buffer[..read_limit])
            .map_err(|error| format!("Cannot read waveform PCM: {error}"))?;
        if read == 0 {
            break;
        }
        if let Some(remaining) = &mut remaining_data {
            *remaining = remaining.saturating_sub(read as u64);
        }
        pending.extend_from_slice(&buffer[..read]);
        let complete_len = pending.len() - (pending.len() % bytes_per_frame);
        let mut offset = 0;
        while offset < complete_len {
            let mut sum = 0.0_f32;
            for channel in 0..usize::from(header.channels) {
                let sample_offset = offset + channel * 2;
                let sample =
                    i16::from_le_bytes([pending[sample_offset], pending[sample_offset + 1]]);
                sum += f32::from(sample) / 32_768.0;
            }
            let sample = (sum / f32::from(header.channels)).clamp(-1.0, 1.0);
            let position = sample_index as f64 / f64::from(header.sample_rate);
            let bin = if duration > 0.0 {
                ((position / duration) * bin_count as f64).floor() as usize
            } else {
                // The editor supplies duration from ffprobe. Keep a useful
                // bounded fallback for direct callers with no duration hint.
                (sample_index / u64::from(header.sample_rate.max(1) / 100).max(1)) as usize
            }
            .min(bin_count - 1);
            mins[bin] = mins[bin].min(sample);
            maxs[bin] = maxs[bin].max(sample);
            sample_index = sample_index.saturating_add(1);
            offset += bytes_per_frame;
        }
        if offset > 0 {
            pending.drain(..offset);
        }
        if remaining_data == Some(0) {
            break;
        }
    }
    let actual_duration = if duration > 0.0 {
        duration
    } else {
        sample_index as f64 / f64::from(header.sample_rate)
    };
    let peaks = mins
        .into_iter()
        .zip(maxs)
        .map(|(min, max)| {
            if min > max {
                Peak::silence()
            } else {
                Peak { min, max }
            }
        })
        .collect();
    Ok(Waveform::from_peaks(actual_duration, peaks, true))
}

struct WavHeader {
    audio_format: u16,
    channels: u16,
    sample_rate: u32,
    block_align: u16,
    bits_per_sample: u16,
    data_size: Option<u64>,
}

fn read_wav_header<R: Read>(reader: &mut R) -> Result<WavHeader, String> {
    let mut riff = [0_u8; 12];
    reader
        .read_exact(&mut riff)
        .map_err(|error| format!("Cannot read waveform WAV header: {error}"))?;
    if &riff[..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err("Waveform extractor returned a non-WAV stream".to_owned());
    }

    let mut format = None;
    loop {
        let mut chunk = [0_u8; 8];
        reader
            .read_exact(&mut chunk)
            .map_err(|error| format!("Cannot read waveform WAV chunk: {error}"))?;
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        if &chunk[..4] == b"fmt " {
            if !(16..=MAX_WAV_CHUNK).contains(&size) {
                return Err("Waveform WAV format chunk is invalid".to_owned());
            }
            let mut bytes = vec![0_u8; size as usize];
            reader
                .read_exact(&mut bytes)
                .map_err(|error| format!("Cannot read waveform WAV format: {error}"))?;
            format = Some(WavHeader {
                audio_format: u16::from_le_bytes([bytes[0], bytes[1]]),
                channels: u16::from_le_bytes([bytes[2], bytes[3]]),
                sample_rate: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
                block_align: u16::from_le_bytes([bytes[12], bytes[13]]),
                bits_per_sample: u16::from_le_bytes([bytes[14], bytes[15]]),
                data_size: None,
            });
            if size % 2 == 1 {
                skip_bytes(reader, 1)?;
            }
        } else if &chunk[..4] == b"data" {
            let Some(mut header) = format else {
                return Err("Waveform WAV data appeared before its format".to_owned());
            };
            // FFmpeg uses 0xffffffff for a streamed data chunk whose final
            // size is unknown until stdout closes.
            header.data_size = (size != u32::MAX).then_some(u64::from(size));
            return Ok(header);
        } else {
            skip_bytes(reader, u64::from(size) + u64::from(size % 2))?;
        }
    }
}

fn skip_bytes<R: Read>(reader: &mut R, mut count: u64) -> Result<(), String> {
    let mut buffer = [0_u8; 8192];
    while count > 0 {
        let read_len = (count as usize).min(buffer.len());
        let read = reader
            .read(&mut buffer[..read_len])
            .map_err(|error| format!("Cannot skip waveform WAV metadata: {error}"))?;
        if read == 0 {
            return Err("Waveform WAV metadata ended unexpectedly".to_owned());
        }
        count -= read as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn source_time_ranges_do_not_show_audio_outside_the_clip() {
        let wave = Waveform::from_peaks(
            4.,
            vec![
                Peak {
                    min: -0.1,
                    max: 0.1,
                },
                Peak {
                    min: -0.2,
                    max: 0.2,
                },
                Peak {
                    min: -0.8,
                    max: 0.8,
                },
                Peak {
                    min: -0.4,
                    max: 0.4,
                },
            ],
            true,
        );
        assert_eq!(wave.peak_between(2., 3.).max, 0.8);
        assert_eq!(wave.peak_between(1.5, 2.5).max, 0.8);
        assert_eq!(wave.peak_between(0., 1.).max, 0.1);
        assert_eq!(wave.peak_between(4., 5.), Peak::silence());
        assert_eq!(wave.peak_between(-2., -1.), Peak::silence());
    }

    fn wav(samples: &[i16], sample_rate: u32) -> Vec<u8> {
        let data_len = samples.len() as u32 * 2;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn reduces_pcm_to_full_duration_bins_and_preserves_polarity() {
        let data = wav(&[-32_768, 16_384, 0, 32_767], 4);
        let waveform = read_waveform(
            Cursor::new(data),
            1.0,
            1,
            &AtomicU64::new(1),
            &AtomicBool::new(false),
        )
        .expect("valid WAV");
        assert_eq!(waveform.duration(), 1.0);
        assert!(waveform.has_audio());
        assert_eq!(waveform.peaks().len(), MIN_BINS);
        let first_column = waveform.peak_for_column(0, 2);
        assert!(first_column.min < -0.99);
        assert!(first_column.max > 0.49);
        assert!(waveform.peak_for_column(1, 2).max > 0.99);
    }

    #[test]
    fn silence_and_column_sampling_are_stable() {
        let waveform = Waveform::silence(3.0);
        assert!(!waveform.has_audio());
        assert_eq!(waveform.peaks().len(), MIN_BINS);
        assert_eq!(waveform.peak_for_column(0, 4), Peak::silence());
        assert_eq!(waveform.peak_for_column(10, 0), Peak::silence());
    }

    #[test]
    fn streamed_ffmpeg_wav_metadata_chunks_are_skipped() {
        let mut data = wav(&[0, 32_767], 2);
        let list = b"LIST\x04\x00\x00\x00INFO";
        data.splice(12..12, list.iter().copied());
        let waveform = read_waveform(
            Cursor::new(data),
            1.0,
            1,
            &AtomicU64::new(1),
            &AtomicBool::new(false),
        )
        .expect("WAV metadata should be skipped");
        assert!(waveform.has_audio());
    }
}
