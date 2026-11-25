//! Whisper transcription of one track with word-level timestamps.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use earshot_protocol::{Backend, ErrorCode, Segment, Stage, TranscribeJob, Transcript, Word};
use whisper_rs::{
    DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext,
    WhisperContextParameters,
};

use crate::{audio, vad};

/// GPU backend compiled into this binary.
pub fn gpu_backend() -> Option<&'static str> {
    if cfg!(feature = "cuda") {
        Some("cuda")
    } else if cfg!(feature = "metal") {
        Some("metal")
    } else {
        None
    }
}

pub struct JobError {
    pub code: ErrorCode,
    pub message: String,
    pub suggestion: Option<String>,
}

impl JobError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            suggestion: None,
        }
    }
    pub fn hint(mut self, s: &str) -> Self {
        self.suggestion = Some(s.into());
        self
    }
}

fn dtw_preset(name: &str) -> Option<DtwModelPreset> {
    Some(match name {
        "tiny.en" => DtwModelPreset::TinyEn,
        "tiny" => DtwModelPreset::Tiny,
        "base.en" => DtwModelPreset::BaseEn,
        "base" => DtwModelPreset::Base,
        "small.en" => DtwModelPreset::SmallEn,
        "small" => DtwModelPreset::Small,
        "medium.en" => DtwModelPreset::MediumEn,
        "medium" => DtwModelPreset::Medium,
        "large-v3" => DtwModelPreset::LargeV3,
        "large-v3-turbo" => DtwModelPreset::LargeV3Turbo,
        _ => return None,
    })
}

pub fn run(
    job: &TranscribeJob,
    cancel: &Arc<AtomicBool>,
    mut progress: impl FnMut(Stage, f32) + Send + 'static,
) -> Result<Transcript, JobError> {
    let started = Instant::now();
    let use_gpu = match job.backend {
        Backend::Cpu => false,
        Backend::Auto => gpu_backend().is_some(),
        Backend::Gpu => {
            if gpu_backend().is_none() {
                return Err(JobError::new(
                    ErrorCode::GpuFailed,
                    "this worker was built without GPU support",
                ));
            }
            true
        }
    };

    // 1. Decode + resample.
    let (samples, rate) = audio::decode_mono(Path::new(&job.audio_path), |p| {
        progress(Stage::Decode, p * 0.9)
    })
    .map_err(|e| JobError::new(ErrorCode::AudioUnreadable, e))?;
    let samples =
        audio::to_16k(samples, rate).map_err(|e| JobError::new(ErrorCode::AudioUnreadable, e))?;
    progress(Stage::Decode, 100.0);
    let audio_ms = samples.len() as u64 * 1000 / u64::from(audio::TARGET_RATE);
    if cancel.load(Ordering::Relaxed) {
        return Err(JobError::new(ErrorCode::Cancelled, "cancelled"));
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8) as i32;

    // 2. Keep only speech (if a VAD model is given).
    let compacted = match &job.vad_model_path {
        Some(vad_path) => {
            progress(Stage::Vad, 0.0);
            let vad_threads = std::thread::available_parallelism().map_or(4, |n| n.get());
            let regions = vad::detect(vad_path, &samples, vad_threads)
                .map_err(|e| JobError::new(ErrorCode::ModelUnreadable, e))?;
            let c = vad::compact(&samples, &regions);
            progress(Stage::Vad, 100.0);
            if c.is_empty() {
                // No speech at all: nothing to transcribe.
                return Ok(Transcript {
                    language: job.language.clone().unwrap_or_else(|| "und".into()),
                    backend_used: "none".into(),
                    audio_ms,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    segments: Vec::new(),
                });
            }
            Some(c)
        }
        None => None,
    };
    let input: &[f32] = compacted.as_ref().map_or(&samples, |c| &c.samples);
    let to_orig = |ms: u64| compacted.as_ref().map_or(ms, |c| c.to_original_ms(ms));
    let word_to_orig = |s: u64, e: u64| {
        compacted
            .as_ref()
            .map_or((s, e), |c| c.word_to_original_ms(s, e))
    };

    // 3. Load the model.
    progress(Stage::LoadModel, 0.0);
    let mut cparams = WhisperContextParameters::default();
    cparams.use_gpu(use_gpu);
    let preset = job.dtw_preset.as_deref().and_then(dtw_preset);
    if let Some(p) = preset.clone() {
        cparams.dtw_parameters(DtwParameters {
            mode: DtwMode::ModelPreset { model_preset: p },
            ..Default::default()
        });
    }
    let ctx = WhisperContext::new_with_params(&job.model_path, cparams).map_err(|e| {
        if use_gpu {
            JobError::new(
                ErrorCode::GpuFailed,
                format!("loading the model on the GPU failed: {e}"),
            )
            .hint("Try again on the CPU, or pick a smaller model.")
        } else {
            JobError::new(
                ErrorCode::ModelUnreadable,
                format!("cannot load the model: {e}"),
            )
            .hint("Delete the model in Settings → Transcription and download it again.")
        }
    })?;
    let mut state = ctx.create_state().map_err(|e| {
        let code = if use_gpu {
            ErrorCode::GpuFailed
        } else {
            ErrorCode::TranscribeFailed
        };
        JobError::new(code, format!("cannot create inference state: {e}"))
    })?;
    progress(Stage::LoadModel, 100.0);

    // 4. Transcribe.
    let mut params = FullParams::new(SamplingStrategy::BeamSearch {
        beam_size: 5,
        patience: -1.0,
    });
    params.set_n_threads(threads);
    params.set_language(Some(job.language.as_deref().unwrap_or("auto")));
    params.set_token_timestamps(true);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_special(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    params.set_progress_callback_safe(move |p: i32| progress(Stage::Transcribe, p as f32));
    // whisper-rs 0.16's `set_abort_callback_safe` stores the closure as `Box<Box<dyn FnMut>>` but
    // its trampoline reads it back as the concrete closure type (undefined behaviour that makes
    // whisper abort the encoder at random). Use the raw API with a trampoline that reads the
    // cancel flag directly; `cancel` outlives `state.full` below.
    unsafe extern "C" fn abort_trampoline(user_data: *mut std::ffi::c_void) -> bool {
        // SAFETY: user_data is the `AtomicBool` inside `cancel`, alive for the whole call.
        unsafe { (*(user_data as *const AtomicBool)).load(Ordering::Relaxed) }
    }
    // SAFETY: the callback only reads an AtomicBool that lives until `full` returns.
    unsafe {
        params.set_abort_callback(Some(abort_trampoline));
        params.set_abort_callback_user_data(Arc::as_ptr(cancel) as *mut std::ffi::c_void);
    }

    state.full(params, input).map_err(|e| {
        if cancel.load(Ordering::Relaxed) {
            JobError::new(ErrorCode::Cancelled, "cancelled")
        } else if use_gpu {
            JobError::new(
                ErrorCode::GpuFailed,
                format!("transcription on the GPU failed: {e}"),
            )
            .hint("Try again on the CPU.")
        } else {
            JobError::new(
                ErrorCode::TranscribeFailed,
                format!("transcription failed: {e}"),
            )
        }
    })?;

    let eot = ctx.token_eot();
    let use_dtw = preset.is_some();
    let mut segments = Vec::new();
    for i in 0..state.full_n_segments() {
        let Some(seg) = state.get_segment(i) else {
            continue;
        };
        let seg_start = seg.start_timestamp().max(0) as u64 * 10;
        let seg_end = (seg.end_timestamp().max(0) as u64 * 10).max(seg_start);
        let mut pieces = Vec::new();
        for t in 0..seg.n_tokens() {
            let Some(tok) = seg.get_token(t) else {
                continue;
            };
            if tok.token_id() >= eot {
                continue; // special and timestamp tokens
            }
            let bytes = tok.to_bytes().unwrap_or_default().to_vec();
            if bytes.starts_with(b"[_") {
                continue;
            }
            let d = tok.token_data();
            pieces.push(Piece {
                bytes,
                t0: d.t0,
                t1: d.t1,
                t_dtw: d.t_dtw,
                p: d.p,
            });
        }
        let mut words = assemble_words(&pieces, seg_start, seg_end, use_dtw);
        // Back to the original track timeline (VAD removed the silences).
        for w in &mut words {
            (w.start_ms, w.end_ms) = word_to_orig(w.start_ms, w.end_ms);
        }
        let (seg_start, seg_end) = (to_orig(seg_start), to_orig(seg_end));
        let text = seg
            .to_str_lossy()
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        if text.is_empty() && words.is_empty() {
            continue;
        }
        segments.push(Segment {
            start_ms: seg_start,
            end_ms: seg_end,
            text,
            words,
        });
    }
    let language = whisper_rs::get_lang_str(state.full_lang_id_from_state())
        .unwrap_or("und")
        .to_owned();

    Ok(Transcript {
        language,
        backend_used: if use_gpu {
            gpu_backend().unwrap_or("cpu")
        } else {
            "cpu"
        }
        .to_owned(),
        audio_ms,
        elapsed_ms: started.elapsed().as_millis() as u64,
        segments,
    })
}

/// Longest a single word may last when its end comes from DTW (next word's start).
const MAX_WORD_MS: u64 = 1_000;

/// One text token from whisper (times in centiseconds; `t_dtw` is -1 when unavailable).
#[derive(Debug, Clone)]
pub struct Piece {
    pub bytes: Vec<u8>,
    pub t0: i64,
    pub t1: i64,
    pub t_dtw: i64,
    pub p: f32,
}

/// Groups sub-word tokens into words (a token starting with a space starts a new word) and
/// assigns times: DTW times when available (start = first token's DTW time, end = next word's
/// start), otherwise the tokens' own t0/t1. Times are clamped to the segment and kept monotonic.
pub fn assemble_words(pieces: &[Piece], seg_start: u64, seg_end: u64, use_dtw: bool) -> Vec<Word> {
    struct Acc {
        bytes: Vec<u8>,
        start: u64,
        end: u64,
        p_sum: f32,
        n: u32,
    }
    let cs = |t: i64| (t.max(0) as u64) * 10;
    let mut acc: Vec<Acc> = Vec::new();
    for piece in pieces {
        let starts_word = acc.is_empty() || piece.bytes.first() == Some(&b' ');
        let start = if use_dtw && piece.t_dtw >= 0 {
            cs(piece.t_dtw)
        } else {
            cs(piece.t0)
        };
        if starts_word {
            acc.push(Acc {
                bytes: Vec::new(),
                start,
                end: cs(piece.t1),
                p_sum: 0.0,
                n: 0,
            });
        }
        let w = acc.last_mut().expect("pushed above");
        w.bytes.extend_from_slice(&piece.bytes);
        w.end = w.end.max(cs(piece.t1));
        w.p_sum += piece.p;
        w.n += 1;
    }
    if use_dtw {
        // A DTW time marks where a token begins; a word ends where the next one begins, but no
        // later than MAX_WORD_MS after its start (so the word before a pause doesn't swallow it).
        for i in 0..acc.len() {
            let next = acc.get(i + 1).map(|n| n.start).unwrap_or(seg_end);
            acc[i].end = next.min(acc[i].start + MAX_WORD_MS);
        }
    }
    let mut words = Vec::with_capacity(acc.len());
    let mut prev_end = seg_start;
    for a in acc {
        let text = String::from_utf8_lossy(&a.bytes).into_owned();
        if text.trim().is_empty() {
            continue;
        }
        let start = a.start.clamp(prev_end, seg_end);
        let end = a.end.clamp(start, seg_end);
        prev_end = start;
        words.push(Word {
            start_ms: start,
            end_ms: end,
            text,
            prob: if a.n > 0 { a.p_sum / a.n as f32 } else { 0.0 },
        });
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str, t0: i64, t1: i64, dtw: i64) -> Piece {
        Piece {
            bytes: text.as_bytes().to_vec(),
            t0,
            t1,
            t_dtw: dtw,
            p: 0.9,
        }
    }

    #[test]
    fn merges_subword_tokens_and_uses_dtw() {
        let pieces = [
            p(" Hel", 0, 10, 2),
            p("lo", 10, 20, 8),
            p(" world", 20, 40, 25),
            p(".", 40, 42, 39),
        ];
        let w = assemble_words(&pieces, 0, 500, true);
        assert_eq!(
            w.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
            [" Hello", " world."]
        );
        assert_eq!((w[0].start_ms, w[0].end_ms), (20, 250));
        assert_eq!((w[1].start_ms, w[1].end_ms), (250, 500));
        // The last word before a long pause is capped.
        let w = assemble_words(
            &[p(" so", 0, 10, 0), p(" anyway", 900, 950, 900)],
            0,
            10_000,
            true,
        );
        assert_eq!((w[0].end_ms, w[1].end_ms), (1_000, 10_000));
    }

    #[test]
    fn falls_back_to_token_times_and_stays_monotonic() {
        let pieces = [
            p(" a", 100, 120, -1),
            p(" b", 90, 150, -1),
            p(" c", 600, 700, -1),
        ];
        let w = assemble_words(&pieces, 1000, 6500, false);
        assert_eq!(w.len(), 3);
        assert!(w.windows(2).all(|x| x[0].start_ms <= x[1].start_ms));
        assert!(
            w.iter()
                .all(|x| x.start_ms >= 1000 && x.end_ms <= 6500 && x.start_ms <= x.end_ms)
        );
        assert_eq!(w[2].start_ms, 6000);
    }

    #[test]
    fn keeps_multibyte_characters_split_across_tokens() {
        let euro = "€".as_bytes();
        let pieces = [
            Piece {
                bytes: [b" ".as_slice(), &euro[..1]].concat(),
                t0: 0,
                t1: 5,
                t_dtw: -1,
                p: 1.0,
            },
            Piece {
                bytes: euro[1..].to_vec(),
                t0: 5,
                t1: 10,
                t_dtw: -1,
                p: 1.0,
            },
        ];
        assert_eq!(assemble_words(&pieces, 0, 100, false)[0].text, " €");
    }
}
