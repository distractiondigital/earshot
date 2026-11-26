//! Hardware detection for model recommendations. Read-only, local, never sent anywhere.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    pub name: String,
    pub vram_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hardware {
    pub cpu: String,
    pub cores: usize,
    pub ram_bytes: u64,
    /// NVIDIA GPU (via NVML), if present.
    pub nvidia: Option<Gpu>,
    pub apple_silicon: bool,
    /// GPU backend compiled into the transcription worker ("cuda", "metal"), if any.
    pub worker_gpu: Option<String>,
}

pub fn detect(worker_gpu: Option<String>) -> Hardware {
    let (cpu, cores, ram_bytes) = match sysinfo::System::new() {
        Ok(mut sys) => {
            sys.refresh_memory();
            sys.refresh_cpu_all();
            let cpu = sys
                .cpus()
                .first()
                .map(|c| c.brand().trim().to_owned())
                .unwrap_or_default();
            (cpu, sys.cpus().len(), sys.total_memory())
        }
        Err(_) => (
            String::new(),
            std::thread::available_parallelism().map_or(4, |n| n.get()),
            0,
        ),
    };
    Hardware {
        cpu,
        cores,
        ram_bytes,
        nvidia: nvidia(),
        apple_silicon: cfg!(all(target_os = "macos", target_arch = "aarch64")),
        worker_gpu,
    }
}

#[cfg(not(target_os = "macos"))]
fn nvidia() -> Option<Gpu> {
    // NVML ships with the NVIDIA driver; failing to load it just means "no NVIDIA GPU".
    let nvml = nvml_wrapper::Nvml::init().ok()?;
    let dev = nvml.device_by_index(0).ok()?;
    Some(Gpu {
        name: dev.name().ok()?,
        vram_bytes: dev.memory_info().ok()?.total,
    })
}

#[cfg(target_os = "macos")]
fn nvidia() -> Option<Gpu> {
    None
}

const GB: u64 = 1024 * 1024 * 1024;

/// The transcription model to mark "Recommended for this PC".
pub fn recommend(hw: &Hardware) -> &'static str {
    match hw.worker_gpu.as_deref() {
        Some("cuda") => match hw.nvidia.as_ref().map(|g| g.vram_bytes) {
            Some(v) if v >= 6 * GB => "large-v3-turbo",
            Some(v) if v >= 4 * GB => "large-v3-turbo-q5_0",
            Some(_) => "small-q5_1",
            None => cpu_pick(hw),
        },
        // Apple Silicon: GPU shares system memory.
        Some("metal") if hw.ram_bytes >= 16 * GB => "large-v3-turbo",
        Some("metal") => "large-v3-turbo-q5_0",
        _ => cpu_pick(hw),
    }
}

/// The summary model to recommend: the largest that runs comfortably. Leaves room for the
/// context window and the desktop (the GPU is also driving the display).
pub fn recommend_summary(hw: &Hardware) -> &'static str {
    match hw.worker_gpu.as_deref() {
        Some("cuda") => match hw.nvidia.as_ref().map(|g| g.vram_bytes) {
            Some(v) if v >= 10 * GB => "qwen3.5-9b",
            Some(v) if v >= 5 * GB => "qwen3.5-4b",
            Some(_) => "qwen3.5-2b",
            None => cpu_summary(hw),
        },
        Some("metal") if hw.ram_bytes >= 16 * GB => "qwen3.5-9b",
        Some("metal") => "qwen3.5-4b",
        _ => cpu_summary(hw),
    }
}

/// On the CPU, speed matters more than polish: a long meeting is a lot of reading.
fn cpu_summary(hw: &Hardware) -> &'static str {
    if hw.cores >= 8 && hw.ram_bytes >= 16 * GB {
        "qwen3.5-4b"
    } else {
        "qwen3.5-2b"
    }
}

fn cpu_pick(hw: &Hardware) -> &'static str {
    if hw.cores >= 8 {
        "small-q5_1"
    } else {
        "base-q5_1"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(worker_gpu: Option<&str>, vram_gb: Option<u64>, ram_gb: u64, cores: usize) -> Hardware {
        Hardware {
            cpu: String::new(),
            cores,
            ram_bytes: ram_gb * GB,
            nvidia: vram_gb.map(|v| Gpu {
                name: "GPU".into(),
                vram_bytes: v * GB,
            }),
            apple_silicon: worker_gpu == Some("metal"),
            worker_gpu: worker_gpu.map(Into::into),
        }
    }

    #[test]
    fn recommendations() {
        assert_eq!(
            recommend(&hw(Some("cuda"), Some(16), 32, 16)),
            "large-v3-turbo"
        );
        assert_eq!(
            recommend(&hw(Some("cuda"), Some(4), 16, 8)),
            "large-v3-turbo-q5_0"
        );
        // NVIDIA GPU but a CPU-only worker build: judge by CPU.
        assert_eq!(recommend(&hw(None, Some(16), 32, 16)), "small-q5_1");
        assert_eq!(
            recommend(&hw(Some("metal"), None, 16, 10)),
            "large-v3-turbo"
        );
        assert_eq!(
            recommend(&hw(Some("metal"), None, 8, 8)),
            "large-v3-turbo-q5_0"
        );
        assert_eq!(recommend(&hw(None, None, 8, 4)), "base-q5_1");
    }

    #[test]
    fn summary_recommendations() {
        assert_eq!(
            recommend_summary(&hw(Some("cuda"), Some(16), 32, 16)),
            "qwen3.5-9b"
        );
        assert_eq!(
            recommend_summary(&hw(Some("cuda"), Some(8), 32, 16)),
            "qwen3.5-4b"
        );
        assert_eq!(
            recommend_summary(&hw(Some("cuda"), Some(4), 16, 8)),
            "qwen3.5-2b"
        );
        assert_eq!(
            recommend_summary(&hw(Some("metal"), None, 16, 10)),
            "qwen3.5-9b"
        );
        assert_eq!(recommend_summary(&hw(None, None, 16, 8)), "qwen3.5-4b");
        assert_eq!(recommend_summary(&hw(None, None, 8, 4)), "qwen3.5-2b");
        for id in ["qwen3.5-2b", "qwen3.5-4b", "qwen3.5-9b"] {
            assert!(crate::models::find(id).is_some_and(|m| m.category == "summary"));
        }
    }
}
