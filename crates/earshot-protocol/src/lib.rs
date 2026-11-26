//! IPC between the Earshot app and `earshot-worker`: newline-delimited JSON over the worker's
//! stdin (requests) and stdout (events). One job runs at a time; the worker exits on stdin EOF.

use serde::{Deserialize, Serialize};

/// App -> worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Ping {
        id: u64,
    },
    Transcribe {
        id: u64,
        job: TranscribeJob,
    },
    /// Splits a track into speaker turns ("who spoke when").
    Diarize {
        id: u64,
        job: DiarizeJob,
    },
    /// Cancels the running job with this id (it ends with an `Error { code: Cancelled }`).
    Cancel {
        id: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// The compiled GPU backend if available, else CPU.
    Auto,
    /// The compiled GPU backend (CUDA / Metal); fails if unavailable.
    Gpu,
    Cpu,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscribeJob {
    /// FLAC track written by the recorder.
    pub audio_path: String,
    /// whisper.cpp GGML model.
    pub model_path: String,
    /// Silero VAD GGML model; `None` transcribes everything (slower, more hallucination on silence).
    pub vad_model_path: Option<String>,
    /// ISO 639-1 code, or `None` to auto-detect.
    pub language: Option<String>,
    pub backend: Backend,
    /// whisper.cpp DTW alignment preset for word timestamps (e.g. "large-v3-turbo").
    pub dtw_preset: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiarizeJob {
    /// FLAC track written by the recorder.
    pub audio_path: String,
    /// pyannote segmentation ONNX model.
    pub segmentation_model_path: String,
    /// Speaker-embedding ONNX model.
    pub embedding_model_path: String,
    /// Known number of speakers; `None` lets `threshold` decide.
    pub num_speakers: Option<u32>,
    /// Clustering distance threshold used when the count is unknown. Larger -> fewer speakers.
    pub threshold: f32,
}

/// Worker -> app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Pong {
        id: u64,
        version: String,
        /// GPU backend compiled into this worker ("cuda", "metal"), if any.
        gpu_backend: Option<String>,
    },
    Progress {
        id: u64,
        stage: Stage,
        /// 0–100 within the stage.
        pct: f32,
    },
    Result {
        id: u64,
        transcript: Transcript,
    },
    /// Result of a `Diarize` request.
    Diarization {
        id: u64,
        diarization: Diarization,
    },
    Error {
        id: u64,
        code: ErrorCode,
        message: String,
        /// Something the user can do about it.
        suggestion: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Decode,
    /// Finding the parts of the track that contain speech.
    Vad,
    LoadModel,
    Transcribe,
    /// Separating speakers.
    Diarize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    AudioUnreadable,
    ModelUnreadable,
    /// The GPU backend failed to initialise or run (retry on CPU).
    GpuFailed,
    TranscribeFailed,
    DiarizeFailed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// Detected or requested language (ISO 639-1).
    pub language: String,
    /// "cuda", "metal" or "cpu".
    pub backend_used: String,
    pub audio_ms: u64,
    pub elapsed_ms: u64,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub words: Vec<Word>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub start_ms: u64,
    pub end_ms: u64,
    /// Includes its leading space, as produced by the model.
    pub text: String,
    pub prob: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diarization {
    pub audio_ms: u64,
    pub elapsed_ms: u64,
    pub num_speakers: u32,
    /// Sorted by start time. Turns of different speakers may overlap.
    pub turns: Vec<SpeakerTurn>,
    /// One voice embedding per speaker (from their clearest speech), for voice profiles.
    #[serde(default)]
    pub embeddings: Vec<SpeakerEmbedding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerEmbedding {
    pub speaker: u32,
    /// L2-normalized, so the dot product of two embeddings is their cosine similarity.
    pub vector: Vec<f32>,
    /// Speech that went into it.
    pub speech_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerTurn {
    pub start_ms: u64,
    pub end_ms: u64,
    /// 0-based cluster index, stable only within one result.
    pub speaker: u32,
}

impl Event {
    pub fn id(&self) -> u64 {
        match self {
            Event::Pong { id, .. }
            | Event::Progress { id, .. }
            | Event::Result { id, .. }
            | Event::Diarization { id, .. }
            | Event::Error { id, .. } => *id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_is_stable() {
        let r: Request = serde_json::from_str(r#"{"type":"cancel","id":3}"#).unwrap();
        assert_eq!(r, Request::Cancel { id: 3 });
        let e = Event::Progress {
            id: 1,
            stage: Stage::Transcribe,
            pct: 50.0,
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"progress","id":1,"stage":"transcribe","pct":50.0}"#
        );
        let ping: Request = serde_json::from_str(r#"{"type":"ping","id":1}"#).unwrap();
        assert_eq!(ping, Request::Ping { id: 1 });
    }
}
