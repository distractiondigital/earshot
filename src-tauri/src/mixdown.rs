//! Mixes a meeting's tracks (mic + meeting audio) into one mono file, FLAC or MP3.
//!
//! Tracks are streamed, never loaded whole, so a long meeting needs little memory. Every track
//! starts at the same moment (see the recorder), so mixing is a sample-by-sample sum; a track
//! recorded at another sample rate is resampled to the highest rate among the tracks first.

use std::fs::File;
use std::path::Path;

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};
use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;

use crate::recorder::flac::FlacWriter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixFormat {
    Flac,
    Mp3,
}

impl MixFormat {
    pub fn extension(self) -> &'static str {
        match self {
            MixFormat::Flac => "flac",
            MixFormat::Mp3 => "mp3",
        }
    }
}

/// MP3 bitrate for the mono mix: speech-clear at about 29 MB per hour.
const MP3_BITRATE: mp3lame_encoder::Bitrate = mp3lame_encoder::Bitrate::Kbps64;
/// Samples mixed per step.
const BLOCK: usize = 4096;

/// Something that yields mono samples. `pull` appends up to `n` samples to `out` and returns how
/// many it added; fewer than `n` means the stream has ended.
trait Pull {
    fn pull(&mut self, out: &mut Vec<f32>, n: usize) -> Result<usize, String>;
}

/// A FLAC track decoded to mono at its own sample rate.
struct Track {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    rate: u32,
    /// Decoded mono samples not handed out yet (from `pos`).
    pending: Vec<f32>,
    pos: usize,
    interleaved: Vec<f32>,
    ended: bool,
}

impl Track {
    fn open(path: &Path) -> Result<Self, String> {
        let file =
            File::open(path).map_err(|e| format!("Couldn't open {}: {e}", path.display()))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let reader =
            symphonia::default::formats::FlacReader::try_new(mss, FormatOptions::default())
                .map_err(|e| format!("{} isn't a readable FLAC file: {e}", path.display()))?;
        let track = reader
            .default_track(TrackType::Audio)
            .ok_or("no audio track")?;
        let params = match track.codec_params.as_ref() {
            Some(CodecParameters::Audio(p)) => p.clone(),
            _ => return Err("not an audio track".into()),
        };
        let rate = params.sample_rate.ok_or("missing sample rate")?;
        let decoder = symphonia::default::codecs::FlacDecoder::try_new(
            &params,
            &AudioDecoderOptions::default(),
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            reader: Box::new(reader),
            decoder: Box::new(decoder),
            rate,
            pending: Vec::new(),
            pos: 0,
            interleaved: Vec::new(),
            ended: false,
        })
    }

    /// Decodes the next packet into `pending`; false at the end of the stream.
    fn decode_next(&mut self) -> bool {
        let packet = match self.reader.next_packet() {
            Ok(Some(p)) => p,
            // The end, or a torn tail (recovered tracks have none): stop either way.
            Ok(None) | Err(_) => return false,
        };
        let buf: GenericAudioBufferRef = match self.decoder.decode(&packet) {
            Ok(b) => b,
            Err(_) => return false,
        };
        let ch = buf.spec().channels().count().max(1);
        self.interleaved.resize(buf.samples_interleaved(), 0.0);
        buf.copy_to_slice_interleaved(&mut self.interleaved);
        self.pending.drain(..self.pos);
        self.pos = 0;
        self.pending.extend(
            self.interleaved
                .chunks_exact(ch)
                .map(|f| f.iter().sum::<f32>() / ch as f32),
        );
        true
    }
}

impl Pull for Track {
    fn pull(&mut self, out: &mut Vec<f32>, n: usize) -> Result<usize, String> {
        while self.pending.len() - self.pos < n && !self.ended {
            if !self.decode_next() {
                self.ended = true;
            }
        }
        let take = n.min(self.pending.len() - self.pos);
        out.extend_from_slice(&self.pending[self.pos..self.pos + take]);
        self.pos += take;
        Ok(take)
    }
}

/// Resamples a mono stream to another rate, keeping its duration.
struct Resampled<S: Pull> {
    source: S,
    resampler: Fft<f32>,
    ratio: f64,
    input: Vec<f32>,
    output: Vec<f32>,
    /// Resampled samples not handed out yet (from `pos`).
    pending: Vec<f32>,
    pos: usize,
    /// Leading output samples still to drop (the resampler's delay).
    skip: usize,
    consumed: u64,
    emitted: u64,
    /// Total output length, known once the source has ended.
    total: Option<u64>,
}

impl<S: Pull> Resampled<S> {
    fn new(source: S, from: u32, to: u32) -> Result<Self, String> {
        let resampler = Fft::<f32>::new(from as usize, to as usize, 1024, 1, FixedSync::Input)
            .map_err(|e| e.to_string())?;
        let skip = resampler.output_delay();
        let output = vec![0.0; resampler.output_frames_max()];
        Ok(Self {
            source,
            resampler,
            ratio: to as f64 / from as f64,
            input: Vec::new(),
            output,
            pending: Vec::new(),
            pos: 0,
            skip,
            consumed: 0,
            emitted: 0,
            total: None,
        })
    }

    /// Runs the resampler once; false when there is nothing more to produce.
    fn step(&mut self) -> Result<bool, String> {
        if self.total.is_some_and(|t| self.emitted >= t) {
            return Ok(false);
        }
        let need = self.resampler.input_frames_next();
        self.input.clear();
        let got = if self.total.is_none() {
            self.source.pull(&mut self.input, need)?
        } else {
            0
        };
        self.consumed += got as u64;
        if got < need && self.total.is_none() {
            self.total = Some((self.consumed as f64 * self.ratio).round() as u64);
        }
        // After the end the resampler is fed silence until its delayed output is all out.
        self.input.resize(need, 0.0);
        let indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            partial_len: (got < need).then_some(got),
            active_channels_mask: None,
        };
        let input = InterleavedSlice::new(&self.input, 1, need).map_err(|e| e.to_string())?;
        let len = self.output.len();
        let mut output =
            InterleavedSlice::new_mut(&mut self.output, 1, len).map_err(|e| e.to_string())?;
        let (_, produced) = self
            .resampler
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(|e| e.to_string())?;
        let mut fresh = &self.output[..produced];
        let skipped = self.skip.min(fresh.len());
        self.skip -= skipped;
        fresh = &fresh[skipped..];
        if let Some(total) = self.total {
            fresh = &fresh[..fresh.len().min((total - self.emitted) as usize)];
        }
        self.emitted += fresh.len() as u64;
        self.pending.drain(..self.pos);
        self.pos = 0;
        self.pending.extend_from_slice(fresh);
        Ok(true)
    }
}

impl<S: Pull> Pull for Resampled<S> {
    fn pull(&mut self, out: &mut Vec<f32>, n: usize) -> Result<usize, String> {
        while self.pending.len() - self.pos < n {
            if !self.step()? {
                break;
            }
        }
        let take = n.min(self.pending.len() - self.pos);
        out.extend_from_slice(&self.pending[self.pos..self.pos + take]);
        self.pos += take;
        Ok(take)
    }
}

/// Keeps the summed tracks from clipping: unchanged below 0.8, then a smooth curve to 1.0.
fn limit(x: f32) -> f32 {
    const KNEE: f32 = 0.8;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        x.signum() * (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh())
    }
}

enum Sink {
    Flac(Box<FlacWriter>),
    Mp3 {
        encoder: mp3lame_encoder::Encoder,
        file: std::io::BufWriter<File>,
        buf: Vec<u8>,
    },
}

/// Sample rates an MP3 can be written at.
const MP3_RATES: [u32; 9] = [
    8_000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000,
];

impl Sink {
    fn create(path: &Path, format: MixFormat, rate: u32, title: &str) -> Result<Self, String> {
        let cant = |e: &dyn std::fmt::Display| format!("Couldn't write {}: {e}", path.display());
        match format {
            MixFormat::Flac => FlacWriter::create(path, rate, 1)
                .map(|w| Sink::Flac(Box::new(w)))
                .map_err(|e| cant(&e)),
            MixFormat::Mp3 => {
                use mp3lame_encoder::{Builder, Id3Tag, Mode, Quality};
                let mut b = Builder::new().ok_or("Couldn't start the MP3 encoder.")?;
                let e = |e: mp3lame_encoder::BuildError| format!("MP3 encoder: {e}");
                b.set_num_channels(1).map_err(e)?;
                b.set_sample_rate(rate).map_err(e)?;
                // LAME resamples itself when the rate isn't one MP3 supports.
                let out_rate = if MP3_RATES.contains(&rate) {
                    rate
                } else {
                    48_000
                };
                b.set_output_sample_rate(std::num::NonZeroU32::new(out_rate))
                    .map_err(e)?;
                b.set_mode(Mode::Mono).map_err(e)?;
                b.set_brate(MP3_BITRATE).map_err(e)?;
                b.set_quality(Quality::NearBest).map_err(e)?;
                // ID3 text is Latin-1, so other characters are left out of the tag.
                let ascii_title: String = title
                    .chars()
                    .filter(|c| c.is_ascii() && !c.is_ascii_control())
                    .collect();
                let _ = b.set_id3_tag(Id3Tag {
                    title: ascii_title.as_bytes(),
                    artist: b"",
                    album: b"",
                    album_art: &[],
                    year: b"",
                    comment: b"Recorded with Earshot",
                });
                let encoder = b.build().map_err(e)?;
                let file = File::create(path).map_err(|e| cant(&e))?;
                Ok(Sink::Mp3 {
                    encoder,
                    file: std::io::BufWriter::with_capacity(256 * 1024, file),
                    buf: Vec::new(),
                })
            }
        }
    }

    fn write(&mut self, samples: &[f32]) -> Result<(), String> {
        use std::io::Write;
        match self {
            Sink::Flac(w) => {
                let ints: Vec<i32> = samples
                    .iter()
                    .map(|s| (s * 32767.0).round() as i32)
                    .collect();
                w.write(&ints).map_err(|e| e.to_string())
            }
            Sink::Mp3 { encoder, file, buf } => {
                buf.clear();
                // LAME's worst case: 1.25 × samples + 7200 bytes.
                buf.reserve(samples.len() * 5 / 4 + 7200);
                encoder
                    .encode_to_vec(mp3lame_encoder::MonoPcm(samples), buf)
                    .map_err(|e| format!("MP3 encoder: {e}"))?;
                file.write_all(buf).map_err(|e| e.to_string())
            }
        }
    }

    fn finish(self) -> Result<(), String> {
        use std::io::Write;
        match self {
            Sink::Flac(w) => w.finish().map(|_| ()).map_err(|e| e.to_string()),
            Sink::Mp3 {
                mut encoder,
                mut file,
                mut buf,
            } => {
                buf.clear();
                buf.reserve(7200);
                encoder
                    .flush_to_vec::<mp3lame_encoder::FlushNoGap>(&mut buf)
                    .map_err(|e| format!("MP3 encoder: {e}"))?;
                file.write_all(&buf).map_err(|e| e.to_string())?;
                let file = file.into_inner().map_err(|e| e.into_error().to_string())?;
                file.sync_all().map_err(|e| e.to_string())
            }
        }
    }
}

/// Mixes `tracks` (FLAC files that start together) into one mono file at `target`.
/// The file is written next to the target first and only renamed into place when complete.
pub fn mix(tracks: &[&Path], target: &Path, format: MixFormat, title: &str) -> Result<(), String> {
    let opened = tracks
        .iter()
        .map(|p| Track::open(p))
        .collect::<Result<Vec<_>, _>>()?;
    let rate = opened
        .iter()
        .map(|t| t.rate)
        .max()
        .ok_or("This meeting has no audio.")?;
    let mut sources: Vec<Box<dyn Pull>> = Vec::new();
    for t in opened {
        if t.rate == rate {
            sources.push(Box::new(t));
        } else {
            let from = t.rate;
            sources.push(Box::new(Resampled::new(t, from, rate)?));
        }
    }

    let partial = target.with_extension(format!("{}.part", format.extension()));
    let result = (|| {
        let mut sink = Sink::create(&partial, format, rate, title)?;
        let mut mixed = vec![0.0f32; BLOCK];
        let mut chunk = Vec::with_capacity(BLOCK);
        loop {
            mixed.fill(0.0);
            let mut longest = 0;
            for s in &mut sources {
                chunk.clear();
                let n = s.pull(&mut chunk, BLOCK)?;
                for (m, c) in mixed.iter_mut().zip(&chunk) {
                    *m += c;
                }
                longest = longest.max(n);
            }
            if longest == 0 {
                break;
            }
            for m in &mut mixed[..longest] {
                *m = limit(*m);
            }
            sink.write(&mixed[..longest])?;
            if longest < BLOCK {
                break;
            }
        }
        sink.finish()
    })();
    match result {
        Ok(()) => std::fs::rename(&partial, target)
            .map_err(|e| format!("Couldn't save {}: {e}", target.display())),
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

/// Decodes a FLAC file to mono samples and its rate (tests elsewhere in the crate).
#[cfg(test)]
pub(crate) fn decode_mono(path: &Path) -> (Vec<f32>, u32) {
    let mut t = Track::open(path).unwrap();
    let mut out = Vec::new();
    while t.pull(&mut out, BLOCK).unwrap() == BLOCK {}
    (out, t.rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, secs: f32, hz: f32, amp: f32) -> Vec<i32> {
        (0..(rate as f32 * secs) as usize)
            .map(|i| {
                ((i as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * amp * 32767.0) as i32
            })
            .collect()
    }

    fn write_track(path: &Path, rate: u32, channels: u16, mono: &[i32]) {
        let mut w = FlacWriter::create(path, rate, channels).unwrap();
        let samples: Vec<i32> = mono
            .iter()
            .flat_map(|&s| std::iter::repeat_n(s, channels as usize))
            .collect();
        w.write(&samples).unwrap();
        w.finish().unwrap();
    }

    fn read_all(path: &Path) -> (Vec<f32>, u32) {
        let mut t = Track::open(path).unwrap();
        let mut out = Vec::new();
        while t.pull(&mut out, BLOCK).unwrap() == BLOCK {}
        (out, t.rate)
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("earshot-mix-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn mixes_tracks_of_different_rates_and_lengths() {
        let d = dir("rates");
        let mic = d.join("mic.flac");
        let sys = d.join("system.flac");
        write_track(&mic, 44_100, 1, &tone(44_100, 2.0, 440.0, 0.3));
        write_track(&sys, 48_000, 2, &tone(48_000, 3.0, 220.0, 0.3));
        let out = d.join("mix.flac");
        mix(&[&mic, &sys], &out, MixFormat::Flac, "Test").unwrap();
        let (samples, rate) = read_all(&out);
        assert_eq!(rate, 48_000);
        // As long as the longest track (3 s at 48 kHz).
        assert!(
            (samples.len() as i64 - 144_000).abs() <= 2,
            "{}",
            samples.len()
        );
        // Both tones while both play, then the meeting track alone.
        let peak = |s: &[f32]| s.iter().fold(0.0f32, |a, x| a.max(x.abs()));
        assert!(peak(&samples[24_000..72_000]) > 0.45);
        let tail = peak(&samples[100_000..140_000]);
        assert!((0.25..0.35).contains(&tail), "{tail}");
        assert!(!d.join("mix.flac.part").exists());
    }

    #[test]
    fn writes_an_mp3() {
        let d = dir("mp3");
        let mic = d.join("mic.flac");
        write_track(&mic, 48_000, 1, &tone(48_000, 2.0, 440.0, 0.5));
        let out = d.join("mix.mp3");
        mix(&[&mic], &out, MixFormat::Mp3, "Test").unwrap();
        let bytes = std::fs::read(&out).unwrap();
        // ID3 tag, then MPEG frames; 64 kbps for 2 s is about 16 KB.
        assert_eq!(&bytes[..3], b"ID3");
        assert!((12_000..24_000).contains(&bytes.len()), "{}", bytes.len());
    }

    #[test]
    fn limiter_keeps_samples_in_range() {
        assert_eq!(limit(0.5), 0.5);
        assert!(limit(1.6) < 1.0 && limit(1.6) > 0.95);
        assert!((-1.0..-0.99).contains(&limit(-3.0)));
    }
}
