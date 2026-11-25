//! Voice activity detection: only speech is sent to Whisper (faster, and no hallucinated text on
//! long silences). Speech regions are padded, merged, and joined with short silences; a mapping
//! table converts times in the joined audio back to the original track timeline.
//!
//! whisper.cpp has this built in, but only in `whisper_full`; whisper-rs drives
//! `whisper_full_with_state`, which ignores the VAD settings, so it is done here instead.

use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};

use crate::audio::TARGET_RATE;

/// Context kept around each detected speech region.
const PAD_MS: u64 = 200;
/// Silence inserted between regions in the joined audio.
const GAP_MS: u64 = 100;

fn ms_to_samples(ms: u64) -> usize {
    (ms * u64::from(TARGET_RATE) / 1000) as usize
}

fn samples_to_ms(n: usize) -> u64 {
    n as u64 * 1000 / u64::from(TARGET_RATE)
}

/// One speech region placed in the joined audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Piece {
    joined_start: usize,
    orig_start: usize,
    len: usize,
}

/// Speech-only audio plus the table that maps it back to the original timeline.
#[derive(Debug, Default)]
pub struct Compacted {
    pub samples: Vec<f32>,
    pieces: Vec<Piece>,
}

impl Compacted {
    /// Maps a time (ms) in the joined audio to the original track timeline.
    pub fn to_original_ms(&self, joined_ms: u64) -> u64 {
        let t = ms_to_samples(joined_ms);
        let idx = self.pieces.partition_point(|p| p.joined_start <= t);
        let Some(p) = idx.checked_sub(1).map(|i| self.pieces[i]) else {
            return self
                .pieces
                .first()
                .map_or(joined_ms, |p| samples_to_ms(p.orig_start));
        };
        // Inside the region, or in the inserted gap after it (clamped to the region's end).
        let offset = (t - p.joined_start).min(p.len);
        samples_to_ms(p.orig_start + offset)
    }

    /// Maps a word's (start, end) in the joined audio to the original timeline. The end is kept
    /// within the speech region the word starts in: a word never spans the silence that was cut
    /// out, even when its end time (taken from the next word's start) falls in the next region.
    pub fn word_to_original_ms(&self, start: u64, end: u64) -> (u64, u64) {
        let s = self.to_original_ms(start);
        let t = ms_to_samples(start);
        let idx = self.pieces.partition_point(|p| p.joined_start <= t);
        let region_end = idx
            .checked_sub(1)
            .map(|i| samples_to_ms(self.pieces[i].orig_start + self.pieces[i].len))
            .unwrap_or(u64::MAX);
        let e = self.to_original_ms(end).min(region_end).max(s);
        (s, e)
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }
}

/// Joins the given speech regions (sample ranges in `samples`), padding and merging them.
pub fn compact(samples: &[f32], regions: &[(usize, usize)]) -> Compacted {
    let pad = ms_to_samples(PAD_MS);
    let gap = ms_to_samples(GAP_MS);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for &(s, e) in regions {
        let (s, e) = (s.saturating_sub(pad), (e + pad).min(samples.len()));
        if s >= e {
            continue;
        }
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    let mut out = Compacted::default();
    for (i, (s, e)) in merged.into_iter().enumerate() {
        if i > 0 {
            out.samples.extend(std::iter::repeat_n(0.0, gap));
        }
        out.pieces.push(Piece {
            joined_start: out.samples.len(),
            orig_start: s,
            len: e - s,
        });
        out.samples.extend_from_slice(&samples[s..e]);
    }
    out
}

/// Runs Silero VAD over 16 kHz mono audio and returns speech regions as sample ranges.
///
/// Silero processes audio sequentially in small windows, so one context barely benefits from
/// more threads (~80 s per hour of audio). The audio is split into chunks analysed in parallel,
/// one VAD context each; speech cut at a chunk boundary is re-joined by [`compact`]'s padding.
/// (whisper.cpp's GPU VAD path aborts in 1.8.3, so this stays on the CPU.)
pub fn detect(
    model_path: &str,
    samples: &[f32],
    threads: usize,
) -> Result<Vec<(usize, usize)>, String> {
    const MIN_CHUNK_SECS: usize = 60;
    let min_chunk = MIN_CHUNK_SECS * TARGET_RATE as usize;
    let n = threads.max(1).min(samples.len().div_ceil(min_chunk)).max(1);
    let chunk = samples.len().div_ceil(n).max(1);
    let results: Vec<Result<Vec<(usize, usize)>, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = samples
            .chunks(chunk)
            .enumerate()
            .map(|(i, part)| {
                scope.spawn(move || detect_one(model_path, part).map(|r| offset(r, i * chunk)))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("VAD thread panicked".into()))
            })
            .collect()
    });
    let mut regions = Vec::new();
    for r in results {
        regions.extend(r?);
    }
    regions.sort_unstable();
    Ok(regions)
}

fn offset(regions: Vec<(usize, usize)>, by: usize) -> Vec<(usize, usize)> {
    regions.into_iter().map(|(s, e)| (s + by, e + by)).collect()
}

fn detect_one(model_path: &str, samples: &[f32]) -> Result<Vec<(usize, usize)>, String> {
    let mut cparams = WhisperVadContextParams::new();
    cparams.set_n_threads(1);
    cparams.set_use_gpu(false);
    let mut ctx = WhisperVadContext::new(model_path, cparams)
        .map_err(|e| format!("cannot load VAD model: {e}"))?;
    let segments = ctx
        .segments_from_samples(WhisperVadParams::default(), samples)
        .map_err(|e| format!("VAD failed: {e}"))?;
    // Segment times are in centiseconds.
    let cs_to_samples = |cs: f32| ((cs.max(0.0) as f64) * f64::from(TARGET_RATE) / 100.0) as usize;
    Ok(segments
        .map(|s| {
            (
                cs_to_samples(s.start).min(samples.len()),
                cs_to_samples(s.end).min(samples.len()),
            )
        })
        .filter(|(s, e)| e > s)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_regions_and_maps_times_back() {
        let sr = TARGET_RATE as usize;
        let audio = vec![0.1f32; sr * 30];
        // Speech at 1–3 s and 20–22 s.
        let c = compact(&audio, &[(sr, 3 * sr), (20 * sr, 22 * sr)]);
        // 0.8–3.2 s (2.4 s) + 0.1 s gap + 19.8–22.2 s (2.4 s)
        assert_eq!(c.samples.len(), ms_to_samples(2400 + 100 + 2400));
        assert_eq!(c.to_original_ms(0), 800);
        assert_eq!(c.to_original_ms(1200), 2000);
        // Inside the inserted gap: clamped to the end of the first region.
        assert_eq!(c.to_original_ms(2450), 3200);
        // Start of the second region.
        assert_eq!(c.to_original_ms(2500), 19_800);
        assert_eq!(c.to_original_ms(3700), 21_000);
        // A word starting at the end of region 1 whose end falls in region 2 stays in region 1.
        assert_eq!(c.word_to_original_ms(2300, 2600), (3100, 3200));
        assert_eq!(c.word_to_original_ms(2600, 2900), (19_900, 20_200));
    }

    #[test]
    fn merges_overlapping_padding_and_clamps_to_audio() {
        let sr = TARGET_RATE as usize;
        let audio = vec![0.0f32; sr * 2];
        let c = compact(&audio, &[(0, sr / 2), (sr / 2 + 100, sr * 2)]);
        assert_eq!(c.pieces.len(), 1);
        assert_eq!(c.samples.len(), sr * 2);
        assert!(compact(&audio, &[]).is_empty());
    }
}
