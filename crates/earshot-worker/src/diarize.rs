//! Speaker diarization ("who spoke when") of one track with sherpa-onnx:
//! pyannote segmentation -> speaker embeddings -> clustering.
//!
//! Uses the C API directly (through `sherpa-onnx-sys`) because the safe wrapper doesn't expose the
//! progress callback, and embedding extraction takes most of the time on long meetings.

use std::ffi::{CString, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use earshot_protocol::{Diarization, DiarizeJob, ErrorCode, SpeakerEmbedding, SpeakerTurn, Stage};
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
            embeddings: Vec::new(),
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
    let (num_speakers, turns) = turns;
    let embeddings = match speaker_embeddings(&job.embedding_model_path, &samples, &turns) {
        Ok(e) => e,
        Err(e) => {
            // Voice profiles are a bonus; speaker labels still work without them.
            log::warn!("speaker embeddings failed: {e}");
            Vec::new()
        }
    };
    progress(Stage::Diarize, 100.0);
    Ok(Diarization {
        audio_ms,
        elapsed_ms: started.elapsed().as_millis() as u64,
        num_speakers,
        turns,
        embeddings,
    })
}

/// Turns shorter than this (after trimming) don't give a reliable embedding.
const MIN_EMBED_MS: u64 = 1_500;
/// Speech used per speaker; more barely changes the embedding but costs time.
const MAX_EMBED_MS: u64 = 30_000;
/// Trimmed from each end of a turn: speaker changes are fuzzy at the edges.
const EDGE_TRIM_MS: u64 = 200;

/// Picks each speaker's clearest speech: their longest turns that don't overlap anyone else's,
/// trimmed at the edges, up to [`MAX_EMBED_MS`]. Returns (speaker, start ms, end ms) spans.
pub fn embedding_spans(turns: &[SpeakerTurn]) -> Vec<(u32, u64, u64)> {
    let mut by_speaker: std::collections::BTreeMap<u32, Vec<(u64, u64)>> = Default::default();
    for (i, t) in turns.iter().enumerate() {
        // Cut away any part another speaker overlaps.
        let mut pieces = vec![(
            t.start_ms + EDGE_TRIM_MS,
            t.end_ms.saturating_sub(EDGE_TRIM_MS),
        )];
        for (j, o) in turns.iter().enumerate() {
            if i == j || o.speaker == t.speaker {
                continue;
            }
            pieces = pieces
                .into_iter()
                .flat_map(|(s, e)| {
                    if o.end_ms <= s || o.start_ms >= e {
                        vec![(s, e)]
                    } else {
                        vec![(s, o.start_ms.max(s)), (o.end_ms.min(e), e)]
                    }
                })
                .filter(|(s, e)| e > s)
                .collect();
        }
        by_speaker
            .entry(t.speaker)
            .or_default()
            .extend(pieces.into_iter().filter(|(s, e)| e - s >= MIN_EMBED_MS));
    }
    let mut out = Vec::new();
    for (speaker, mut spans) in by_speaker {
        spans.sort_by_key(|(s, e)| std::cmp::Reverse(e - s));
        let mut total = 0;
        for (s, e) in spans {
            if total >= MAX_EMBED_MS {
                break;
            }
            let e = e.min(s + (MAX_EMBED_MS - total));
            total += e - s;
            out.push((speaker, s, e));
        }
    }
    out
}

/// One L2-normalized embedding per speaker: the duration-weighted mean of its spans' embeddings.
fn speaker_embeddings(
    model: &str,
    samples: &[f32],
    turns: &[SpeakerTurn],
) -> Result<Vec<SpeakerEmbedding>, String> {
    let spans = embedding_spans(turns);
    if spans.is_empty() {
        return Ok(Vec::new());
    }
    let model = CString::new(model).map_err(|_| "path contains NUL")?;
    let cpu = CString::new("cpu").expect("no NUL");
    let config = sys::SpeakerEmbeddingExtractorConfig {
        model: model.as_ptr(),
        num_threads: threads(),
        debug: 0,
        provider: cpu.as_ptr(),
    };
    // SAFETY: config's strings outlive the call; the extractor is destroyed below.
    let ex = unsafe { sys::SherpaOnnxCreateSpeakerEmbeddingExtractor(&config) };
    if ex.is_null() {
        return Err("couldn't load the embedding model".into());
    }
    // SAFETY: `ex` is valid.
    let dim = unsafe { sys::SherpaOnnxSpeakerEmbeddingExtractorDim(ex) }.max(0) as usize;
    let mut sums: std::collections::BTreeMap<u32, (Vec<f32>, u64)> = Default::default();
    for (speaker, s, e) in spans {
        let from = (s as usize * SAMPLE_RATE / 1000).min(samples.len());
        let to = (e as usize * SAMPLE_RATE / 1000).min(samples.len());
        if to <= from {
            continue;
        }
        let chunk = &samples[from..to];
        // SAFETY: `ex` is valid; the stream and the returned vector are freed before the next
        // iteration; `chunk` outlives the AcceptWaveform call.
        let v = unsafe {
            let stream = sys::SherpaOnnxSpeakerEmbeddingExtractorCreateStream(ex);
            if stream.is_null() {
                continue;
            }
            sys::SherpaOnnxOnlineStreamAcceptWaveform(
                stream,
                SAMPLE_RATE as i32,
                chunk.as_ptr(),
                chunk.len() as i32,
            );
            sys::SherpaOnnxOnlineStreamInputFinished(stream);
            let out = if sys::SherpaOnnxSpeakerEmbeddingExtractorIsReady(ex, stream) != 0 {
                let p = sys::SherpaOnnxSpeakerEmbeddingExtractorComputeEmbedding(ex, stream);
                if p.is_null() {
                    None
                } else {
                    let v = std::slice::from_raw_parts(p, dim).to_vec();
                    sys::SherpaOnnxSpeakerEmbeddingExtractorDestroyEmbedding(p);
                    Some(v)
                }
            } else {
                None
            };
            sys::SherpaOnnxDestroyOnlineStream(stream);
            out
        };
        let Some(v) = v.map(normalize) else { continue };
        let w = (e - s) as f32;
        let entry = sums.entry(speaker).or_insert_with(|| (vec![0.0; dim], 0));
        for (acc, x) in entry.0.iter_mut().zip(&v) {
            *acc += x * w;
        }
        entry.1 += e - s;
    }
    // SAFETY: created above, destroyed once.
    unsafe { sys::SherpaOnnxDestroySpeakerEmbeddingExtractor(ex) };
    Ok(sums
        .into_iter()
        .map(|(speaker, (v, speech_ms))| SpeakerEmbedding {
            speaker,
            vector: normalize(v),
            speech_ms,
        })
        .collect())
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in &mut v {
            *x /= n;
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: u64, e: u64, speaker: u32) -> SpeakerTurn {
        SpeakerTurn {
            start_ms: s,
            end_ms: e,
            speaker,
        }
    }

    #[test]
    fn embedding_spans_avoid_overlap_and_short_bits() {
        let turns = [
            t(0, 10_000, 0),
            t(9_000, 12_000, 1),  // overlaps speaker 0 for a second
            t(20_000, 21_000, 1), // too short
            t(30_000, 70_000, 0), // long: capped
        ];
        let spans = embedding_spans(&turns);
        // Speaker 0: longest first, capped at 30 s in total.
        let s0: Vec<_> = spans.iter().filter(|s| s.0 == 0).collect();
        assert_eq!(s0[0], &(0, 30_200, 60_200));
        assert_eq!(s0.iter().map(|s| s.2 - s.1).sum::<u64>(), MAX_EMBED_MS);
        // Speaker 1: only the part after speaker 0 stops, trimmed; the 1 s turn is dropped.
        assert_eq!(
            spans.iter().filter(|s| s.0 == 1).collect::<Vec<_>>(),
            [&(1, 10_000, 11_800)]
        );
    }
}
