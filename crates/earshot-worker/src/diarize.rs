//! Speaker diarization ("who spoke when") of one track with sherpa-onnx:
//! pyannote segmentation -> speaker embeddings -> clustering.
//!
//! Uses the C API directly (through `sherpa-onnx-sys`) because the safe wrapper doesn't expose the
//! progress callback, and embedding extraction takes most of the time on long meetings.

use std::ffi::{CString, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use earshot_protocol::{Diarization, DiarizeJob, ErrorCode, SpeakerTurn, Stage};
use sherpa_onnx_sys as sys;

use crate::audio;
use crate::transcribe::JobError;

/// sherpa-onnx's pyannote pipeline expects 16 kHz mono.
const SAMPLE_RATE: usize = 16_000;
/// Step between 10 s segmentation windows, as a share of the window. sherpa-onnx defaults to 0.1;
/// on the AMI benchmark 0.3 was 3x faster with under one point more diarization error, and less
/// sensitive to the clustering threshold.
const WINDOW_SHIFT: f32 = 0.3;

/// Owns a diarizer handle.
struct Diarizer(*const sys::OfflineSpeakerDiarization);

impl Drop for Diarizer {
    fn drop(&mut self) {
        // SAFETY: created by SherpaOnnxCreateOfflineSpeakerDiarization and dropped once.
        unsafe { sys::SherpaOnnxDestroyOfflineSpeakerDiarization(self.0) }
    }
}

unsafe extern "C" {
    // Present in the C API but not declared by sherpa-onnx-sys 2.0.3.
    fn SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback(
        sd: *const sys::OfflineSpeakerDiarization,
        samples: *const f32,
        n: i32,
        callback: Option<unsafe extern "C" fn(i32, i32, *mut c_void) -> i32>,
        arg: *mut c_void,
    ) -> *const sys::OfflineSpeakerDiarizationResult;
}

/// Calls the progress closure passed as `arg`.
unsafe extern "C" fn on_progress(done: i32, total: i32, arg: *mut c_void) -> i32 {
    // SAFETY: `arg` is the `&mut dyn FnMut` passed below and outlives the call.
    let f = unsafe { &mut *(arg as *mut &mut dyn FnMut(i32, i32)) };
    f(done, total);
    0 // ignored by sherpa-onnx 2.0.3
}

fn threads() -> i32 {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(8) as i32)
}

pub fn run(
    job: &DiarizeJob,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Stage, f32),
) -> Result<Diarization, JobError> {
    let started = Instant::now();
    let check = |cancel: &AtomicBool| {
        if cancel.load(Ordering::Relaxed) {
            Err(JobError::new(ErrorCode::Cancelled, "cancelled"))
        } else {
            Ok(())
        }
    };

    for p in [&job.segmentation_model_path, &job.embedding_model_path] {
        if !Path::new(p).is_file() {
            return Err(
                JobError::new(ErrorCode::ModelUnreadable, format!("model not found: {p}"))
                    .hint("Download the speaker models again in Settings → Speakers."),
            );
        }
    }

    let (samples, rate) = audio::decode_mono(Path::new(&job.audio_path), |p| {
        progress(Stage::Decode, p * 0.8)
    })
    .map_err(|e| JobError::new(ErrorCode::AudioUnreadable, e))?;
    check(cancel)?;
    let samples =
        audio::to_16k(samples, rate).map_err(|e| JobError::new(ErrorCode::AudioUnreadable, e))?;
    progress(Stage::Decode, 100.0);
    check(cancel)?;
    let audio_ms = (samples.len() * 1000 / SAMPLE_RATE) as u64;

    let cstr = |s: &str| {
        CString::new(s).map_err(|_| JobError::new(ErrorCode::BadRequest, "path contains NUL"))
    };
    let seg = cstr(&job.segmentation_model_path)?;
    let emb = cstr(&job.embedding_model_path)?;
    let cpu = cstr("cpu")?;
    let config = sys::OfflineSpeakerDiarizationConfig {
        segmentation: sys::OfflineSpeakerSegmentationModelConfig {
            pyannote: sys::OfflineSpeakerSegmentationPyannoteModelConfig {
                model: seg.as_ptr(),
                window_shift_ratio: WINDOW_SHIFT,
            },
            num_threads: threads(),
            debug: 0,
            provider: cpu.as_ptr(),
        },
        embedding: sys::SpeakerEmbeddingExtractorConfig {
            model: emb.as_ptr(),
            num_threads: threads(),
            debug: 0,
            provider: cpu.as_ptr(),
        },
        clustering: sys::FastClusteringConfig {
            num_clusters: job.num_speakers.map_or(-1, |n| n as i32),
            threshold: job.threshold,
            compute_confidence: 0,
        },
        min_duration_on: 0.3,
        min_duration_off: 0.5,
    };
    // SAFETY: every pointer in `config` points into CStrings that live until the end of this function.
    let sd = unsafe { sys::SherpaOnnxCreateOfflineSpeakerDiarization(&config) };
    if sd.is_null() {
        return Err(JobError::new(
            ErrorCode::ModelUnreadable,
            "couldn't load the speaker models",
        )
        .hint("Download the speaker models again in Settings → Speakers."));
    }
    let sd = Diarizer(sd);
    check(cancel)?;

    // Diarizing a few seconds of audio is meaningless; return one speaker.
    if samples.len() < SAMPLE_RATE {
        return Ok(Diarization {
            audio_ms,
            elapsed_ms: started.elapsed().as_millis() as u64,
            num_speakers: 0,
            turns: Vec::new(),
        });
    }

    progress(Stage::Diarize, 0.0);
    let mut report = |done: i32, total: i32| {
        if total > 0 {
            progress(Stage::Diarize, done as f32 * 100.0 / total as f32);
        }
    };
    let mut report_dyn: &mut dyn FnMut(i32, i32) = &mut report;
    // SAFETY: `sd` is valid, `samples` outlives the call, and `report_dyn` lives on this stack frame
    // for the duration of the call (the callback is not retained afterwards).
    let result = unsafe {
        SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback(
            sd.0,
            samples.as_ptr(),
            samples.len() as i32,
            Some(on_progress),
            &mut report_dyn as *mut &mut dyn FnMut(i32, i32) as *mut c_void,
        )
    };
    if result.is_null() {
        return Err(JobError::new(
            ErrorCode::DiarizeFailed,
            "speaker separation failed",
        ));
    }
    // SAFETY: `result` is a valid result until destroyed below; the segment array has
    // `num_segments` entries until destroyed.
    let turns = unsafe {
        let num_speakers = sys::SherpaOnnxOfflineSpeakerDiarizationResultGetNumSpeakers(result);
        let n =
            sys::SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments(result).max(0) as usize;
        let segs = sys::SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime(result);
        let turns = if segs.is_null() {
            Vec::new()
        } else {
            let turns = std::slice::from_raw_parts(segs, n)
                .iter()
                .map(|s| SpeakerTurn {
                    start_ms: (s.start.max(0.0) * 1000.0).round() as u64,
                    end_ms: (s.end.max(0.0) * 1000.0).round() as u64,
                    speaker: s.speaker.max(0) as u32,
                })
                .collect::<Vec<_>>();
            sys::SherpaOnnxOfflineSpeakerDiarizationDestroySegment(segs);
            turns
        };
        sys::SherpaOnnxOfflineSpeakerDiarizationDestroyResult(result);
        (num_speakers.max(0) as u32, turns)
    };
    check(cancel)?;
    progress(Stage::Diarize, 100.0);
    Ok(Diarization {
        audio_ms,
        elapsed_ms: started.elapsed().as_millis() as u64,
        num_speakers: turns.0,
        turns: turns.1,
    })
}
