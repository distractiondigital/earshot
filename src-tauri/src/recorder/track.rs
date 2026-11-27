//! Per-track writer thread: drains captured samples from a lock-free ring buffer and appends them
//! to a crash-safe FLAC file.
//!
//! Timeline: every track is aligned to the shared meeting [`Clock`]. Some sources deliver nothing
//! while silent (WASAPI loopback sends no packets when nothing is playing), so the writer pads
//! with digital silence whenever a track falls behind the clock. Samples arriving while the
//! meeting is paused are discarded, and paused time does not advance the clock.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

use super::flac::FlacWriter;

/// How often the writer wakes up to drain the ring buffer.
const TICK: Duration = Duration::from_millis(20);
const FLUSH_EVERY: Duration = Duration::from_secs(1);
const SYNC_EVERY: Duration = Duration::from_secs(5);
/// A track this far behind the clock (with nothing buffered) gets padded with silence.
const SILENCE_SLACK: Duration = Duration::from_millis(250);
/// Padding stops this far short of the clock so late-arriving audio is not pushed out of place.
const SILENCE_MARGIN: Duration = Duration::from_millis(100);

/// Meeting clock shared by all tracks. Paused time is excluded.
#[derive(Debug)]
pub struct Clock {
    state: Mutex<ClockState>,
}

#[derive(Debug)]
struct ClockState {
    started: Instant,
    paused_at: Option<Instant>,
    paused_total: Duration,
}

impl Clock {
    pub fn start() -> Self {
        Self::start_at(Duration::ZERO)
    }

    /// A clock that already reads `offset` (continuing an interrupted meeting).
    pub fn start_at(offset: Duration) -> Self {
        let now = Instant::now();
        Self {
            state: Mutex::new(ClockState {
                started: now.checked_sub(offset).unwrap_or(now),
                paused_at: None,
                paused_total: Duration::ZERO,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ClockState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Recording time so far, excluding pauses.
    pub fn elapsed(&self) -> Duration {
        let s = self.lock();
        let now = s.paused_at.unwrap_or_else(Instant::now);
        now.saturating_duration_since(s.started)
            .saturating_sub(s.paused_total)
    }

    pub fn is_paused(&self) -> bool {
        self.lock().paused_at.is_some()
    }

    pub fn pause(&self) {
        let mut s = self.lock();
        if s.paused_at.is_none() {
            s.paused_at = Some(Instant::now());
        }
    }

    pub fn resume(&self) {
        let mut s = self.lock();
        if let Some(at) = s.paused_at.take() {
            s.paused_total += at.elapsed();
        }
    }
}

/// Live level meter value (0.0–1.0) shared with the UI, stored as f32 bits.
#[derive(Debug, Default)]
pub struct Level(AtomicU32);

impl Level {
    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// Output channel `o` (of `out` channels) from one input frame.
fn remix(frame: &[f32], o: usize, out: usize) -> f32 {
    match (frame.len(), out) {
        (i, o_n) if i == o_n => frame[o],
        // Down to mono: average.
        (i, 1) => frame.iter().sum::<f32>() / i as f32,
        // Up from mono, or other mismatches: reuse input channels cyclically.
        (i, _) => frame[o % i],
    }
}

/// Maps RMS amplitude to a 0–1 meter value over a -60..0 dBFS range.
fn meter_value(rms: f32) -> f32 {
    if rms <= 1e-6 {
        return 0.0;
    }
    ((20.0 * rms.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Turns captured samples (any channel count, any rate) into the track file's format.
struct Converter {
    in_ch: usize,
    out_ch: usize,
    resample: Option<Resample>,
}

/// Rate conversion for a source that runs at a different rate from the file (a device switched
/// to mid-meeting, e.g. a Bluetooth headset at 16 kHz on a 48 kHz track).
struct Resample {
    resampler: Fft<f32>,
    /// Remixed input frames waiting for a full resampler chunk (interleaved, file channels).
    pending: Vec<f32>,
    output: Vec<f32>,
    /// Leading output frames still to drop (the resampler's delay), so audio stays in time.
    skip: usize,
}

impl Converter {
    fn new(in_ch: usize, in_rate: u32, out_ch: usize, out_rate: u32) -> Self {
        let resample = (in_rate != out_rate)
            .then(|| {
                Fft::<f32>::new(
                    in_rate as usize,
                    out_rate as usize,
                    1024,
                    out_ch,
                    FixedSync::Input,
                )
                .map_err(|e| log::error!("can't resample {in_rate} Hz to {out_rate} Hz: {e}"))
                .ok()
            })
            .flatten()
            .map(|resampler| Resample {
                skip: resampler.output_delay(),
                output: vec![0.0; resampler.output_frames_max() * out_ch],
                pending: Vec::new(),
                resampler,
            });
        Self {
            in_ch: in_ch.max(1),
            out_ch,
            resample,
        }
    }

    /// Appends `input` (interleaved, `in_ch` channels) to `out` in the file's format.
    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let (in_ch, out_ch) = (self.in_ch, self.out_ch);
        let frames = input.chunks_exact(in_ch);
        let Some(rs) = self.resample.as_mut() else {
            for frame in frames {
                out.extend((0..out_ch).map(|o| remix(frame, o, out_ch)));
            }
            return;
        };
        for frame in frames {
            rs.pending
                .extend((0..out_ch).map(|o| remix(frame, o, out_ch)));
        }
        loop {
            let need = rs.resampler.input_frames_next();
            if rs.pending.len() < need * out_ch {
                break;
            }
            let len = rs.output.len() / out_ch;
            let result = InterleavedSlice::new(&rs.pending[..need * out_ch], out_ch, need)
                .map_err(|e| e.to_string())
                .and_then(|inp| {
                    let mut outp = InterleavedSlice::new_mut(&mut rs.output, out_ch, len)
                        .map_err(|e| e.to_string())?;
                    rs.resampler
                        .process_into_buffer(&inp, &mut outp, None)
                        .map_err(|e| e.to_string())
                });
            let produced = match result {
                Ok((_, produced)) => produced,
                Err(e) => {
                    log::error!("resampling failed: {e}");
                    rs.pending.clear();
                    break;
                }
            };
            rs.pending.drain(..need * out_ch);
            let skipped = rs.skip.min(produced);
            rs.skip -= skipped;
            out.extend_from_slice(&rs.output[skipped * out_ch..produced * out_ch]);
        }
    }
}

/// Where a writer reads its samples from. Replaced when the device is switched mid-meeting.
struct Input {
    consumer: rtrb::Consumer<f32>,
    converter: Converter,
    buf: Vec<f32>,
}

impl Input {
    fn new(
        consumer: rtrb::Consumer<f32>,
        channels: u16,
        rate: u32,
        out_ch: usize,
        out_rate: u32,
    ) -> Self {
        Self {
            consumer,
            converter: Converter::new(usize::from(channels), rate, out_ch, out_rate),
            buf: Vec::new(),
        }
    }

    /// Moves everything buffered (whole frames only) into `out`, in the file's format.
    fn drain(&mut self, out: &mut Vec<f32>) {
        let in_ch = self.converter.in_ch;
        let available = self.consumer.slots() - self.consumer.slots() % in_ch;
        if available == 0 {
            return;
        }
        let chunk = self
            .consumer
            .read_chunk(available)
            .expect("slots were counted");
        let (a, b) = chunk.as_slices();
        self.buf.clear();
        self.buf.extend_from_slice(a);
        self.buf.extend_from_slice(b);
        chunk.commit_all();
        self.converter.push(&self.buf, out);
    }
}

/// A new source (consumer, channels, sample rate) waiting to be picked up by the writer thread.
type PendingInput = Arc<Mutex<Option<(rtrb::Consumer<f32>, u16, u32)>>>;

pub struct TrackStats {
    pub frames: u64,
    pub sample_rate: u32,
}

pub struct TrackWriter {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<io::Result<u64>>>,
    pub level: Arc<Level>,
    /// Samples dropped because the ring buffer was full (writer too slow).
    pub overflow: Arc<AtomicU64>,
    pub sample_rate: u32,
    pub channels: u16,
    next_input: PendingInput,
}

impl TrackWriter {
    /// Spawns the writer thread. `consumer` yields interleaved f32 samples with `in_channels`
    /// channels at the file's sample rate; they are remixed if the file has a different count
    /// (a continued meeting whose device changed between mono and stereo).
    pub fn spawn(
        name: &str,
        mut flac: FlacWriter,
        in_channels: u16,
        consumer: rtrb::Consumer<f32>,
        overflow: Arc<AtomicU64>,
        clock: Arc<Clock>,
    ) -> io::Result<Self> {
        let sample_rate = flac.sample_rate();
        let channels = flac.channels();
        let stop = Arc::new(AtomicBool::new(false));
        let level = Arc::new(Level::default());
        let next_input: PendingInput = Arc::default();
        let (stop_t, level_t, next_t) = (stop.clone(), level.clone(), next_input.clone());
        let ch = usize::from(channels);
        let rate = f64::from(sample_rate);
        let mut input = Input::new(consumer, in_channels, sample_rate, ch, sample_rate);

        let join = std::thread::Builder::new()
            .name(format!("earshot-writer-{name}"))
            .spawn(move || -> io::Result<u64> {
                let mut samples: Vec<f32> = Vec::with_capacity(sample_rate as usize * ch);
                let mut scratch: Vec<i32> = Vec::with_capacity(sample_rate as usize * ch);
                let mut last_flush = Instant::now();
                let mut last_sync = Instant::now();
                loop {
                    let stopping = stop_t.load(Ordering::Acquire);
                    let paused = clock.is_paused();

                    // Drain everything currently buffered; on a device switch, finish the old
                    // source (already stopped) and carry on from the new one.
                    samples.clear();
                    input.drain(&mut samples);
                    let next = next_t.lock().unwrap_or_else(|e| e.into_inner()).take();
                    if let Some((consumer, in_ch, in_rate)) = next {
                        input.drain(&mut samples);
                        input = Input::new(consumer, in_ch, in_rate, ch, sample_rate);
                        input.drain(&mut samples);
                    }
                    scratch.clear();
                    let mut sum_sq = 0.0f32;
                    for &s in &samples {
                        let s = if s.is_finite() {
                            s.clamp(-1.0, 1.0)
                        } else {
                            0.0
                        };
                        sum_sq += s * s;
                        scratch.push((s * 32767.0).round() as i32);
                    }
                    let rms = if scratch.is_empty() {
                        0.0
                    } else {
                        (sum_sq / scratch.len() as f32).sqrt()
                    };
                    level_t.set(if paused { 0.0 } else { meter_value(rms) });

                    if !paused {
                        // Pad silence if this track has fallen behind the meeting clock.
                        let target = clock.elapsed().as_secs_f64() * rate;
                        let have = (flac.frames_written() + (scratch.len() / ch) as u64) as f64;
                        if target - have > SILENCE_SLACK.as_secs_f64() * rate {
                            let pad = target - have - SILENCE_MARGIN.as_secs_f64() * rate;
                            flac.write_silence(pad as u64)?;
                        }
                        flac.write(&scratch)?;
                    }

                    if stopping {
                        return flac.finish();
                    }
                    if last_sync.elapsed() >= SYNC_EVERY {
                        flac.sync()?;
                        last_sync = Instant::now();
                        last_flush = last_sync;
                    } else if last_flush.elapsed() >= FLUSH_EVERY {
                        flac.flush()?;
                        last_flush = Instant::now();
                    }
                    std::thread::sleep(TICK);
                }
            })?;

        Ok(Self {
            stop,
            join: Some(join),
            level,
            overflow,
            sample_rate,
            channels,
            next_input,
        })
    }

    /// Switches the writer to a new source (`channels` at `rate`), converted to the file's
    /// format. Stop the old capture first: whatever it left in its buffer is still written.
    pub fn replace_input(&self, consumer: rtrb::Consumer<f32>, channels: u16, rate: u32) {
        *self.next_input.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((consumer, channels, rate));
    }

    /// Writes out everything still buffered, finalizes the file and returns its stats.
    pub fn finish(mut self) -> io::Result<TrackStats> {
        self.stop.store(true, Ordering::Release);
        let frames = self
            .join
            .take()
            .expect("finish called once")
            .join()
            .map_err(|_| io::Error::other("writer thread panicked"))??;
        Ok(TrackStats {
            frames,
            sample_rate: self.sample_rate,
        })
    }
}

impl Drop for TrackWriter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Producer side handed to capture code: pushes interleaved samples, counting any that do not fit.
pub struct SampleSink {
    producer: rtrb::Producer<f32>,
    overflow: Arc<AtomicU64>,
}

impl SampleSink {
    #[cfg(test)]
    pub fn push(&mut self, samples: &[f32]) {
        let n = samples.len().min(self.producer.slots());
        if n > 0
            && let Ok(chunk) = self.producer.write_chunk_uninit(n)
        {
            let written = chunk.fill_from_iter(samples[..n].iter().copied());
            debug_assert_eq!(written, n);
        }
        if n < samples.len() {
            self.overflow
                .fetch_add((samples.len() - n) as u64, Ordering::Relaxed);
        }
    }

    /// Pushes samples produced by `f` one at a time (for format conversion without allocating).
    pub fn push_iter(&mut self, len: usize, f: impl Iterator<Item = f32>) {
        let n = len.min(self.producer.slots());
        if n > 0
            && let Ok(chunk) = self.producer.write_chunk_uninit(n)
        {
            chunk.fill_from_iter(f.take(n));
        }
        if n < len {
            self.overflow.fetch_add((len - n) as u64, Ordering::Relaxed);
        }
    }
}

/// Creates a ring buffer holding `seconds` of audio.
pub fn ring(
    sample_rate: u32,
    channels: u16,
    seconds: u32,
) -> (SampleSink, rtrb::Consumer<f32>, Arc<AtomicU64>) {
    let cap = sample_rate as usize * usize::from(channels) * seconds as usize;
    let (producer, consumer) = rtrb::RingBuffer::new(cap);
    let overflow = Arc::new(AtomicU64::new(0));
    (
        SampleSink {
            producer,
            overflow: overflow.clone(),
        },
        consumer,
        overflow,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("earshot-track-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("track.flac")
    }

    #[test]
    fn remix_channels() {
        assert!((remix(&[0.2, 0.4], 0, 1) - 0.3).abs() < 1e-6);
        assert_eq!(remix(&[0.5], 1, 2), 0.5);
        assert_eq!(remix(&[0.1, 0.2], 1, 2), 0.2);
    }

    #[test]
    fn clock_can_start_at_offset() {
        let c = Clock::start_at(Duration::from_secs(90));
        assert!(c.elapsed() >= Duration::from_secs(90) && c.elapsed() < Duration::from_secs(91));
    }

    #[test]
    fn clock_excludes_pauses() {
        let c = Clock::start();
        std::thread::sleep(Duration::from_millis(50));
        c.pause();
        let at_pause = c.elapsed();
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(c.elapsed(), at_pause);
        c.resume();
        std::thread::sleep(Duration::from_millis(30));
        let e = c.elapsed();
        // Lower bound is exact; the upper bound is loose because CI runners oversleep.
        assert!(
            e >= at_pause + Duration::from_millis(25) && e < at_pause + Duration::from_millis(500),
            "{e:?}"
        );
    }

    #[test]
    fn silent_source_is_padded_to_clock() {
        // A source that never delivers anything (idle loopback) still yields a track as long
        // as the recording, minus at most the slack.
        let path = temp_path("pad");
        let (_sink, consumer, overflow) = ring(16_000, 1, 2);
        let clock = Arc::new(Clock::start());
        let w = TrackWriter::spawn(
            "test",
            FlacWriter::create(&path, 16_000, 1).unwrap(),
            1,
            consumer,
            overflow,
            clock.clone(),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(1200));
        // Finishing pads up to the clock at that moment, which a slow runner reaches well
        // after `before`; so the track is bounded by the clock read on both sides of it.
        let before = clock.elapsed().as_secs_f64();
        let stats = w.finish().unwrap();
        let after = clock.elapsed().as_secs_f64();
        let secs = stats.frames as f64 / 16_000.0;
        assert!(
            secs <= after && secs >= before - SILENCE_SLACK.as_secs_f64() - 0.05,
            "{secs} not within {before}..{after}"
        );
    }

    fn tone(rate: u32, channels: usize, secs: f32, hz: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize)
            .flat_map(|i| {
                let v = (i as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * 0.5;
                std::iter::repeat_n(v, channels)
            })
            .collect()
    }

    fn crossings(s: &[f32]) -> usize {
        s.windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count()
    }

    #[test]
    fn switching_source_mid_track_converts_to_the_file_format() {
        // A 48 kHz mono track switches to a 16 kHz stereo device (a Bluetooth headset).
        let path = temp_path("switch");
        let (mut sink, consumer, overflow) = ring(48_000, 1, 2);
        let clock = Arc::new(Clock::start());
        let w = TrackWriter::spawn(
            "test",
            FlacWriter::create(&path, 48_000, 1).unwrap(),
            1,
            consumer,
            overflow,
            clock.clone(),
        )
        .unwrap();
        sink.push(&tone(48_000, 1, 0.5, 440.0));
        let (mut sink2, consumer2, _) = ring(16_000, 2, 2);
        w.replace_input(consumer2, 2, 16_000);
        sink2.push(&tone(16_000, 2, 0.5, 1_000.0));
        std::thread::sleep(Duration::from_millis(1200));
        let stats = w.finish().unwrap();
        assert_eq!(stats.sample_rate, 48_000);

        let (s, rate) = crate::mixdown::decode_mono(&path);
        assert_eq!(rate, 48_000);
        // Padded to the clock (less the writer's margin), so the switch left no gap or overlap.
        assert!(s.len() >= 52_000, "{}", s.len());
        let at = |a: f32, b: f32| &s[(a * 48_000.0) as usize..(b * 48_000.0) as usize];
        // The first source at its own pitch, then the second at its own pitch (resampled, not
        // played at the wrong speed), then silence padded to the clock.
        let first = crossings(at(0.05, 0.45));
        assert!((345..=360).contains(&first), "440 Hz section: {first}");
        let second = crossings(at(0.55, 0.85));
        assert!((590..=610).contains(&second), "1 kHz section: {second}");
        assert!(
            s[s.len() - 2_400..].iter().all(|v| v.abs() < 1e-3),
            "padding is silent"
        );
    }

    #[test]
    fn samples_are_written_and_overflow_counted() {
        let path = temp_path("overflow");
        let (mut sink, consumer, overflow) = ring(8_000, 1, 1);
        // Capacity is 8000 samples; pushing 9000 before the writer exists overflows by 1000.
        sink.push(&vec![0.25f32; 9_000]);
        assert_eq!(overflow.load(Ordering::Relaxed), 1_000);
        let clock = Arc::new(Clock::start());
        let w = TrackWriter::spawn(
            "test",
            FlacWriter::create(&path, 8_000, 1).unwrap(),
            1,
            consumer,
            overflow.clone(),
            clock,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(60));
        let stats = w.finish().unwrap();
        assert!(stats.frames >= 8_000, "{}", stats.frames);
    }

    /// M1 acceptance: a 2-hour recording uses flat memory. Feeds 2 h of audio for a stereo
    /// meeting track and a mono mic track through the real ring buffer -> writer -> FLAC path
    /// (faster than real time) and samples this process's resident memory every 10 minutes
    /// of audio. Run with: cargo test --release -p earshot two_hours -- --ignored --nocapture
    #[test]
    #[ignore = "long-running acceptance test"]
    fn two_hours_of_audio_uses_flat_memory() {
        fn rss() -> u64 {
            let mut sys = sysinfo::System::new().unwrap();
            let pid = sysinfo::Pid::from_u32(std::process::id());
            sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
            sys.process(pid).unwrap().memory()
        }
        const RATE: u32 = 48_000;
        const SECONDS: u64 = 2 * 60 * 60;
        let clock = Arc::new(Clock::start());
        let mut feeds = Vec::new();
        let mut writers = Vec::new();
        for (name, ch) in [("system", 2u16), ("mic", 1u16)] {
            let (sink, consumer, overflow) = ring(RATE, ch, 10);
            let path = temp_path(&format!("2h-{name}"));
            writers.push(
                TrackWriter::spawn(
                    name,
                    FlacWriter::create(&path, RATE, ch).unwrap(),
                    ch,
                    consumer,
                    overflow,
                    clock.clone(),
                )
                .unwrap(),
            );
            feeds.push((sink, ch, path));
        }

        let chunk_frames = RATE as usize / 10; // 100 ms
        let mut samples: Vec<Vec<f32>> = feeds
            .iter()
            .map(|(_, ch, _)| vec![0.0; chunk_frames * usize::from(*ch)])
            .collect();
        let mut rng = 0x1234_5678u32;
        let mut readings = Vec::new();
        let total_chunks = SECONDS * 10;
        for i in 0..total_chunks {
            for (buf, (sink, ch, _)) in samples.iter_mut().zip(feeds.iter_mut()) {
                // Speech-like signal: a wobbling tone plus noise (so FLAC frames vary in size).
                for (f, frame) in buf.chunks_mut(usize::from(*ch)).enumerate() {
                    rng ^= rng << 13;
                    rng ^= rng >> 17;
                    rng ^= rng << 5;
                    let t = (i as usize * chunk_frames + f) as f32 / RATE as f32;
                    let v = 0.2
                        * (t * 220.0 * std::f32::consts::TAU * (1.0 + 0.1 * (t * 0.7).sin())).sin()
                        + 0.02 * ((rng as f32 / u32::MAX as f32) - 0.5);
                    frame.fill(v);
                }
                // Wait for room instead of overflowing (the writer drains every 20 ms).
                while sink.producer.slots() < buf.len() {
                    std::thread::sleep(Duration::from_millis(2));
                }
                sink.push(buf);
            }
            if i % (10 * 60 * 10) == 0 {
                let mb = rss() as f64 / 1_048_576.0;
                println!("audio {:>3} min: RSS {mb:.1} MB", i / 600);
                readings.push(mb);
            }
        }
        let stats: Vec<_> = writers.into_iter().map(|w| w.finish().unwrap()).collect();
        let final_mb = rss() as f64 / 1_048_576.0;
        println!("audio 120 min: RSS {final_mb:.1} MB (finished)");
        readings.push(final_mb);
        for (s, (_, _, path)) in stats.iter().zip(&feeds) {
            assert_eq!(s.frames, SECONDS * u64::from(RATE));
            println!(
                "{}: {:.1} MB",
                path.display(),
                std::fs::metadata(path).unwrap().len() as f64 / 1_048_576.0
            );
        }
        // Ignore the first reading (allocations still warming up); the rest must stay flat.
        let steady = &readings[1..];
        let spread = steady.iter().cloned().fold(f64::MIN, f64::max)
            - steady.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            spread < 8.0,
            "memory grew by {spread:.1} MB over 2 h of audio: {readings:?}"
        );
        for (_, _, path) in &feeds {
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }
    }
}
