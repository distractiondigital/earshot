//! llama.cpp text generation for Qwen chat models.

use std::num::NonZeroU32;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::sampling::LlamaSampler;

use crate::summarize::{Chat, Llm};

/// Prompt tokens evaluated per decode call.
const BATCH: usize = 1024;

/// GPU backend compiled into this binary, if any.
pub fn gpu_backend() -> Option<&'static str> {
    if cfg!(feature = "cuda") {
        Some("cuda")
    } else if cfg!(feature = "metal") {
        Some("metal")
    } else {
        None
    }
}

/// The llama.cpp backend can only be initialised once per process.
pub fn backend() -> Result<&'static LlamaBackend, String> {
    static BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();
    BACKEND
        .get_or_init(|| LlamaBackend::init().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(Clone::clone)
}

pub fn load(path: &str, gpu: bool) -> Result<LlamaModel, String> {
    let params = LlamaModelParams::default().with_n_gpu_layers(if gpu { 999 } else { 0 });
    LlamaModel::load_from_file(backend()?, path, &params).map_err(|e| e.to_string())
}

/// Qwen's chat format, with reasoning switched off (an empty thinking block, as Qwen's own chat
/// template writes when thinking is disabled).
pub fn chatml(chat: &Chat) -> String {
    format!(
        "<|im_start|>system\n{}<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n\
         <|im_start|>assistant\n<think>\n\n</think>\n\n",
        chat.system, chat.user
    )
}

/// Turns streamed bytes into text, holding back a multi-byte character split across tokens.
#[derive(Default)]
pub struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(s) => {
                    out.push_str(s);
                    self.pending.clear();
                    return out;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    out.push_str(std::str::from_utf8(&self.pending[..valid]).expect("valid"));
                    match e.error_len() {
                        // An invalid (not just incomplete) sequence: replace it and go on.
                        Some(n) => {
                            out.push('\u{fffd}');
                            self.pending.drain(..valid + n);
                        }
                        // Incomplete: wait for the rest.
                        None => {
                            self.pending.drain(..valid);
                            return out;
                        }
                    }
                }
            }
        }
    }
}

pub struct Session<'m> {
    model: &'m LlamaModel,
    ctx: LlamaContext<'m>,
    n_ctx: usize,
    cancel: &'m AtomicBool,
    seed: u32,
}

impl<'m> Session<'m> {
    pub fn new(
        model: &'m LlamaModel,
        context_tokens: u32,
        threads: i32,
        gpu: bool,
        cancel: &'m AtomicBool,
    ) -> Result<Self, String> {
        let n_ctx = context_tokens.clamp(2048, model.n_ctx_train().max(2048));
        // On the CPU, keep everything there: llama.cpp otherwise still puts the attention cache
        // and large matrix work on a GPU it can see, which defeats a fallback from a failing GPU.
        let params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(n_ctx))
            .with_n_batch(BATCH as u32)
            .with_n_threads(threads)
            .with_n_threads_batch(threads)
            .with_offload_kqv(gpu)
            .with_op_offload(gpu);
        let ctx = model
            .new_context(backend()?, params)
            .map_err(|e| format!("could not set up the model's memory: {e}"))?;
        Ok(Self {
            model,
            ctx,
            n_ctx: n_ctx as usize,
            cancel,
            seed: 1234,
        })
    }
}

impl Llm for Session<'_> {
    fn count(&self, text: &str) -> usize {
        self.model
            .vocab()
            .tokenize(text.as_bytes(), false, false)
            .len()
    }

    fn context(&self) -> usize {
        self.n_ctx
    }

    fn generate(
        &mut self,
        chat: &Chat,
        max_tokens: usize,
        stream: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        let vocab = self.model.vocab();
        let tokens = vocab.tokenize(chatml(chat).as_bytes(), false, true);
        if tokens.len() + max_tokens > self.n_ctx {
            return Err(format!(
                "the prompt ({} tokens) doesn't fit the model's {}-token memory",
                tokens.len(),
                self.n_ctx
            ));
        }
        self.ctx.clear_kv_cache();

        let mut batch = LlamaBatch::new(BATCH, 1);
        let last = tokens.len() - 1;
        for (c, chunk) in tokens.chunks(BATCH).enumerate() {
            if self.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            batch.clear();
            for (i, &t) in chunk.iter().enumerate() {
                let pos = c * BATCH + i;
                batch
                    .add(t, pos as i32, &[0], pos == last)
                    .map_err(|e| e.to_string())?;
            }
            self.ctx
                .decode(&mut batch)
                .map_err(|e| format!("reading the transcript failed: {e}"))?;
        }

        // Qwen's recommended settings for non-thinking use, with a milder presence penalty
        // (summaries legitimately repeat names and list markers).
        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::penalties(vocab.n_tokens(), 256, 1.0, 0.0, 0.5),
            LlamaSampler::top_k(20),
            LlamaSampler::top_p(0.8, 1),
            LlamaSampler::temp(0.7),
            LlamaSampler::dist(self.seed),
        ]);

        let mut out = String::new();
        let mut utf8 = Utf8Stream::default();
        for pos in (tokens.len()..).take(max_tokens) {
            if self.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let token = sampler.sample(&self.ctx, batch.n_tokens() - 1);
            if vocab.is_eog(token) {
                break;
            }
            let piece = utf8.push(&vocab.token_to_piece(token, false, None));
            if !piece.is_empty() {
                stream(&piece);
                out.push_str(&piece);
            }
            batch.clear();
            batch
                .add(token, pos as i32, &[0], true)
                .map_err(|e| e.to_string())?;
            self.ctx
                .decode(&mut batch)
                .map_err(|e| format!("writing failed: {e}"))?;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_stream_holds_split_characters() {
        let mut s = Utf8Stream::default();
        let bytes = "né✓".as_bytes();
        assert_eq!(s.push(&bytes[..2]), "n");
        assert_eq!(s.push(&bytes[2..4]), "é");
        assert_eq!(s.push(&bytes[4..]), "✓");
        assert_eq!(s.push(&[0xff, b'a']), "\u{fffd}a");
    }

    #[test]
    fn chat_format_disables_thinking() {
        let p = chatml(&Chat {
            system: "S".into(),
            user: "U".into(),
        });
        assert!(p.starts_with("<|im_start|>system\nS<|im_end|>"));
        assert!(p.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n"));
    }
}
