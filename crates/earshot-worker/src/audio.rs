//! Track decoding: FLAC -> mono f32 -> 16 kHz (what Whisper expects).

use std::fs::File;
use std::path::Path;

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;

pub const TARGET_RATE: u32 = 16_000;

/// Decodes a FLAC file to mono f32 at its native rate. `progress` gets 0–100.
pub fn decode_mono(path: &Path, mut progress: impl FnMut(f32)) -> Result<(Vec<f32>, u32), String> {
    let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut reader =
        symphonia::default::formats::FlacReader::try_new(mss, FormatOptions::default())
            .map_err(|e| format!("not a readable FLAC file: {e}"))?;
    let track = reader
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let params = match track.codec_params.as_ref() {
        Some(CodecParameters::Audio(p)) => p.clone(),
        _ => return Err("not an audio track".into()),
    };
    let rate = params.sample_rate.ok_or("missing sample rate")?;
    let total = track.num_frames.unwrap_or(0);
    let mut decoder =
        symphonia::default::codecs::FlacDecoder::try_new(&params, &AudioDecoderOptions::default())
            .map_err(|e| e.to_string())?;

    let mut mono = Vec::with_capacity(total as usize);
    let mut interleaved: Vec<f32> = Vec::new();
    let mut last_pct = -1.0f32;
    loop {
        let packet = match reader.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            // A torn tail (should not happen after recovery) ends decoding rather than failing.
            Err(_) => break,
        };
        let buf: GenericAudioBufferRef = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(_) => break,
        };
        let ch = buf.spec().channels().count().max(1);
        interleaved.resize(buf.samples_interleaved(), 0.0);
        buf.copy_to_slice_interleaved(&mut interleaved);
        mono.extend(
            interleaved
                .chunks_exact(ch)
                .map(|f| f.iter().sum::<f32>() / ch as f32),
        );
        if total > 0 {
            let pct = (mono.len() as f32 / total as f32 * 100.0)
                .min(100.0)
                .floor();
            if pct > last_pct {
                last_pct = pct;
                progress(pct);
            }
        }
    }
    Ok((mono, rate))
}

/// Resamples mono audio to 16 kHz.
pub fn to_16k(samples: Vec<f32>, rate: u32) -> Result<Vec<f32>, String> {
    if rate == TARGET_RATE || samples.is_empty() {
        return Ok(samples);
    }
    let mut resampler = Fft::<f32>::new(
        rate as usize,
        TARGET_RATE as usize,
        1024,
        1,
        FixedSync::Input,
    )
    .map_err(|e| e.to_string())?;
    let input = InterleavedSlice::new(&samples, 1, samples.len()).map_err(|e| e.to_string())?;
    let needed = resampler.process_all_needed_output_len(samples.len());
    let mut out = vec![0.0f32; needed];
    let mut output = InterleavedSlice::new_mut(&mut out, 1, needed).map_err(|e| e.to_string())?;
    let (_, produced) = resampler
        .process_all_into_buffer(&input, &mut output, samples.len(), None)
        .map_err(|e| e.to_string())?;
    out.truncate(produced);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_keeps_duration_and_tone() {
        let rate = 48_000;
        let tone: Vec<f32> = (0..rate)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5)
            .collect();
        let out = to_16k(tone, rate).unwrap();
        assert!((out.len() as i64 - 16_000).abs() <= 2, "{}", out.len());
        // Still a 440 Hz tone: count zero crossings over one second (~880).
        let crossings = out
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        assert!((870..=890).contains(&crossings), "{crossings}");
    }
}
