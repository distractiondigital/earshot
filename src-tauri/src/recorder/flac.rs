//! Crash-safe streaming FLAC writer.
//!
//! The file is valid FLAC at every frame boundary: the 42-byte header (`fLaC` + STREAMINFO) is
//! written first with an unknown total length, then each 4096-sample frame is appended as soon as
//! it is encoded. If the process dies, everything up to the last frame that reached the OS is
//! recoverable with [`recover`], which counts the intact frames, truncates any torn tail and
//! rewrites the header. [`FlacWriter::finish`] does the same for a clean stop.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use flacenc::bitsink::MemSink;
use flacenc::component::{BitRepr, Stream, StreamInfo};
use flacenc::error::Verify;
use flacenc::source::{Fill, FrameBuf};

/// Samples per channel in each FLAC frame (~85 ms at 48 kHz).
pub const BLOCK_SIZE: usize = 4096;
pub const BITS_PER_SAMPLE: usize = 16;
/// `fLaC` marker + STREAMINFO metadata block header (4) + STREAMINFO body (34).
const HEADER_LEN: u64 = 42;

fn invalid(msg: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn header_bytes(info: &StreamInfo) -> io::Result<Vec<u8>> {
    let stream = Stream::with_stream_info(info.clone());
    let mut sink = MemSink::<u8>::new();
    stream.write(&mut sink).map_err(invalid)?;
    let bytes = sink.as_slice().to_vec();
    if bytes.len() as u64 != HEADER_LEN {
        return Err(invalid(format!(
            "unexpected FLAC header length {}",
            bytes.len()
        )));
    }
    Ok(bytes)
}

fn stream_info(sample_rate: u32, channels: u16) -> io::Result<StreamInfo> {
    let mut info = StreamInfo::new(sample_rate as usize, channels as usize, BITS_PER_SAMPLE)
        .map_err(invalid)?;
    info.set_block_sizes(BLOCK_SIZE, BLOCK_SIZE)
        .map_err(invalid)?;
    // 0 = unknown, which decoders accept while the stream is still being written.
    info.set_frame_sizes(0, 0).map_err(invalid)?;
    Ok(info)
}

pub struct FlacWriter {
    file: BufWriter<File>,
    config: flacenc::error::Verified<flacenc::config::Encoder>,
    info: StreamInfo,
    framebuf: FrameBuf,
    sink: MemSink<u8>,
    /// Interleaved samples waiting to fill a whole frame (always < one frame).
    pending: Vec<i32>,
    channels: usize,
    frame_number: usize,
    total_frames: u64,
    min_frame_bytes: usize,
    max_frame_bytes: usize,
}

impl FlacWriter {
    pub fn create(path: &Path, sample_rate: u32, channels: u16) -> io::Result<Self> {
        let info = stream_info(sample_rate, channels)?;
        let mut file = BufWriter::with_capacity(256 * 1024, File::create(path)?);
        file.write_all(&header_bytes(&info)?)?;
        let config = flacenc::config::Encoder::default()
            .into_verified()
            .map_err(|(_, e)| invalid(e))?;
        Ok(Self {
            file,
            config,
            info,
            framebuf: FrameBuf::with_size(channels as usize, BLOCK_SIZE).map_err(invalid)?,
            sink: MemSink::new(),
            pending: Vec::with_capacity(BLOCK_SIZE * channels as usize),
            channels: channels as usize,
            frame_number: 0,
            total_frames: 0,
            min_frame_bytes: usize::MAX,
            max_frame_bytes: 0,
        })
    }

    /// Number of sample frames (per channel) accepted so far, including any not yet encoded.
    pub fn frames_written(&self) -> u64 {
        self.total_frames + (self.pending.len() / self.channels) as u64
    }

    /// Appends interleaved samples (already in 16-bit range).
    pub fn write(&mut self, mut samples: &[i32]) -> io::Result<()> {
        debug_assert_eq!(samples.len() % self.channels, 0);
        let frame_len = BLOCK_SIZE * self.channels;
        while !samples.is_empty() {
            let take = (frame_len - self.pending.len()).min(samples.len());
            self.pending.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.pending.len() == frame_len {
                self.encode_pending()?;
            }
        }
        Ok(())
    }

    /// Appends `frames` sample frames of digital silence.
    pub fn write_silence(&mut self, frames: u64) -> io::Result<()> {
        const CHUNK: usize = 4096;
        let zeros = vec![0i32; CHUNK * self.channels];
        let mut left = frames as usize;
        while left > 0 {
            let n = left.min(CHUNK);
            self.write(&zeros[..n * self.channels])?;
            left -= n;
        }
        Ok(())
    }

    fn encode_pending(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let frames = self.pending.len() / self.channels;
        self.framebuf
            .fill_interleaved(&self.pending)
            .map_err(invalid)?;
        let frame = flacenc::encode_fixed_size_frame(
            &self.config,
            &self.framebuf,
            self.frame_number,
            &self.info,
        )
        .map_err(invalid)?;
        self.sink.clear();
        frame.write(&mut self.sink).map_err(invalid)?;
        let bytes = self.sink.as_slice();
        self.file.write_all(bytes)?;
        self.min_frame_bytes = self.min_frame_bytes.min(bytes.len());
        self.max_frame_bytes = self.max_frame_bytes.max(bytes.len());
        self.frame_number += 1;
        self.total_frames += frames as u64;
        self.pending.clear();
        Ok(())
    }

    /// Hands buffered bytes to the OS. Survives a process crash, not a power loss.
    pub fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }

    /// Flushes and asks the OS to persist the data to disk.
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.file.get_ref().sync_data()
    }

    /// Encodes the final partial frame, writes the complete header and syncs. Returns total frames.
    pub fn finish(mut self) -> io::Result<u64> {
        self.encode_pending()?;
        self.info.set_total_samples(self.total_frames as usize);
        if self.max_frame_bytes > 0 {
            self.info
                .set_frame_sizes(self.min_frame_bytes, self.max_frame_bytes)
                .map_err(invalid)?;
        }
        let header = header_bytes(&self.info)?;
        let mut file = self.file.into_inner().map_err(|e| e.into_error())?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header)?;
        file.sync_all()?;
        Ok(self.total_frames)
    }
}

/// Result of [`recover`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recovered {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
}

/// Repairs a FLAC file left behind by an interrupted [`FlacWriter`]: keeps every intact frame,
/// truncates a torn final frame, and rewrites STREAMINFO with the real length.
pub fn recover(path: &Path) -> io::Result<Recovered> {
    let data = std::fs::read(path)?;
    if data.len() < HEADER_LEN as usize || &data[..4] != b"fLaC" {
        return Err(invalid("not a FLAC file written by Earshot"));
    }
    // STREAMINFO body starts at byte 8; sample rate (20 bits) and channels-1 (3 bits) at offset 10.
    let sample_rate =
        (u32::from(data[18]) << 12) | (u32::from(data[19]) << 4) | (u32::from(data[20]) >> 4);
    let channels = u16::from((data[20] >> 1) & 0x07) + 1;

    let (frames, good_len) = scan_frames(&data[HEADER_LEN as usize..]);
    let good_len = HEADER_LEN + good_len as u64;
    drop(data);

    let mut info = stream_info(sample_rate, channels)?;
    info.set_total_samples(frames as usize);
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.set_len(good_len)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header_bytes(&info)?)?;
    file.sync_all()?;
    Ok(Recovered {
        sample_rate,
        channels,
        frames,
    })
}

/// Parsed fixed-blocksize FLAC frame header.
struct FrameHeaderInfo {
    number: u64,
    block_size: u64,
    len: usize,
}

fn crc8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |mut crc, &b| {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
        crc
    })
}

fn crc16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0u16, |mut crc, &b| {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
        crc
    })
}

/// Parses a frame header at the start of `b`, validating its CRC-8.
fn parse_frame_header(b: &[u8]) -> Option<FrameHeaderInfo> {
    if b.len() < 6 || b[0] != 0xFF || b[1] != 0xF8 || b[3] & 0x01 != 0 {
        return None;
    }
    let bs_code = b[2] >> 4;
    let sr_code = b[2] & 0x0F;
    if bs_code == 0 || sr_code == 0x0F {
        return None;
    }
    // Frame number: UTF-8-style variable-length integer.
    let first = b[4];
    let extra = match first {
        0x00..=0x7F => 0,
        0xC0..=0xDF => 1,
        0xE0..=0xEF => 2,
        0xF0..=0xF7 => 3,
        0xF8..=0xFB => 4,
        0xFC..=0xFD => 5,
        _ => return None,
    };
    let mut number = u64::from(if extra == 0 {
        first
    } else {
        first & (0x7F >> (extra + 1))
    });
    let mut pos = 5;
    for _ in 0..extra {
        let c = *b.get(pos)?;
        if c & 0xC0 != 0x80 {
            return None;
        }
        number = (number << 6) | u64::from(c & 0x3F);
        pos += 1;
    }
    let block_size = match bs_code {
        1 => 192,
        2..=5 => 576u64 << (bs_code - 2),
        6 => {
            pos += 1;
            u64::from(*b.get(pos - 1)?) + 1
        }
        7 => {
            pos += 2;
            ((u64::from(*b.get(pos - 2)?) << 8) | u64::from(*b.get(pos - 1)?)) + 1
        }
        _ => 256u64 << (bs_code - 8),
    };
    pos += match sr_code {
        12 => 1,
        13 | 14 => 2,
        _ => 0,
    };
    let crc = *b.get(pos)?;
    (crc8(&b[..pos]) == crc).then_some(FrameHeaderInfo {
        number,
        block_size,
        len: pos + 1,
    })
}

/// Walks the frames after the header. A frame is intact when its header is valid, its number is
/// the next in sequence, and its CRC-16 matches. Returns (sample frames, byte length) of the
/// intact prefix.
fn scan_frames(data: &[u8]) -> (u64, usize) {
    let mut pos = 0usize;
    let mut samples = 0u64;
    let mut expected = 0u64;
    while let Some(h) = parse_frame_header(&data[pos..]).filter(|h| h.number == expected) {
        // The frame ends where the next valid, in-sequence header starts (or at end of file).
        let mut end = None;
        let mut search = pos + h.len + 2;
        while let Some(off) = data
            .get(search..)
            .and_then(|s| s.windows(2).position(|w| w == [0xFF, 0xF8]))
        {
            let cand = search + off;
            if parse_frame_header(&data[cand..]).is_some_and(|n| n.number == expected + 1)
                && crc16(&data[pos..cand]) == 0
            {
                end = Some(cand);
                break;
            }
            search = cand + 1;
        }
        let end = match end {
            Some(e) => e,
            None if crc16(&data[pos..]) == 0 => data.len(),
            None => break, // torn final frame
        };
        samples += h.block_size;
        expected += 1;
        pos = end;
        if pos == data.len() {
            break;
        }
    }
    (samples, pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("earshot-flac-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("track.flac")
    }

    fn sine(frames: usize, channels: usize, rate: u32) -> Vec<i32> {
        (0..frames)
            .flat_map(|i| {
                let v = ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 12000.0)
                    as i32;
                std::iter::repeat_n(v, channels)
            })
            .collect()
    }

    /// Decodes a FLAC file with symphonia and returns (frames, first-channel samples).
    fn decode(path: &Path) -> (u64, Vec<i32>) {
        use symphonia::core::audio::GenericAudioBufferRef;
        use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
        use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
        use symphonia::core::io::MediaSourceStream;

        let mss = MediaSourceStream::new(Box::new(File::open(path).unwrap()), Default::default());
        let mut reader =
            symphonia::default::formats::FlacReader::try_new(mss, FormatOptions::default())
                .unwrap();
        let track = reader.default_track(TrackType::Audio).unwrap();
        let params = match track.codec_params.as_ref().unwrap() {
            symphonia::core::codecs::CodecParameters::Audio(p) => p.clone(),
            _ => panic!(),
        };
        let mut dec = symphonia::default::codecs::FlacDecoder::try_new(
            &params,
            &AudioDecoderOptions::default(),
        )
        .unwrap();
        let mut out = Vec::new();
        let mut frames = 0;
        while let Ok(Some(packet)) = reader.next_packet() {
            let buf: GenericAudioBufferRef = dec.decode(&packet).unwrap();
            let mut samples = vec![0i32; buf.samples_interleaved()];
            buf.copy_to_slice_interleaved(&mut samples);
            let ch = buf.spec().channels().count();
            out.extend(samples.iter().step_by(ch).map(|s| s >> 16));
            frames += buf.frames() as u64;
        }
        (frames, out)
    }

    #[test]
    fn roundtrip_is_lossless() {
        let path = temp_path("roundtrip");
        let input = sine(48_000 + 123, 2, 48_000);
        let mut w = FlacWriter::create(&path, 48_000, 2).unwrap();
        for chunk in input.chunks(2 * 997) {
            w.write(chunk).unwrap();
        }
        assert_eq!(w.frames_written(), 48_123);
        assert_eq!(w.finish().unwrap(), 48_123);
        let (frames, left) = decode(&path);
        assert_eq!(frames, 48_123);
        let expected: Vec<i32> = input.iter().step_by(2).copied().collect();
        assert_eq!(left, expected);
    }

    #[test]
    fn silence_is_counted() {
        let path = temp_path("silence");
        let mut w = FlacWriter::create(&path, 16_000, 1).unwrap();
        w.write_silence(10_000).unwrap();
        w.write(&sine(500, 1, 16_000)).unwrap();
        assert_eq!(w.finish().unwrap(), 10_500);
        assert_eq!(decode(&path).0, 10_500);
    }

    #[test]
    fn recovers_unfinished_file_with_torn_tail() {
        let path = temp_path("torn");
        let mut w = FlacWriter::create(&path, 48_000, 1).unwrap();
        w.write(&sine(BLOCK_SIZE * 10, 1, 48_000)).unwrap();
        w.flush().unwrap();
        let intact_len = std::fs::metadata(&path).unwrap().len();
        // One more full frame, then simulate a crash that tears it: the writer is never
        // finished and the file is cut in the middle of frame 11.
        w.write(&sine(BLOCK_SIZE + 100, 1, 48_000)).unwrap();
        w.flush().unwrap();
        std::mem::forget(w);
        let torn_len = std::fs::metadata(&path).unwrap().len();
        assert!(torn_len > intact_len);
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(intact_len + (torn_len - intact_len) / 2)
            .unwrap();

        let r = recover(&path).unwrap();
        assert_eq!(
            r,
            Recovered {
                sample_rate: 48_000,
                channels: 1,
                frames: (BLOCK_SIZE * 10) as u64
            }
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), intact_len);
        assert_eq!(decode(&path).0, (BLOCK_SIZE * 10) as u64);
        // Recovering again is a no-op.
        assert_eq!(recover(&path).unwrap(), r);
    }
}
