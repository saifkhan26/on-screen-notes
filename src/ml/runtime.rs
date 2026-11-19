//! ONNX Runtime, loaded from the model folder the first time picture
//! search needs it, and a session of the vision model on the GPU
//! (DirectML, any DirectX 12 card) or the CPU.
//!
//! Measured 2026-11-18 on an RTX 2060 / Ryzen 5 3600X, per picture in
//! batches, half-size model: GPU 4–10 ms, CPU 50–55 ms on 4 threads. Both
//! give the vectors a full-size run gives (cosine ≥ 0.997). On the CPU
//! only ONNX Runtime's basic graph rewrites are used: a later fusion
//! (`SimplifiedLayerNormFusion`) breaks on half-size models.

use super::prep::{unit, DIM, LEN, SIDE};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;
use std::sync::OnceLock;

/// Where the model runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Gpu,
    Cpu,
}

impl Device {
    pub fn label(self) -> &'static str {
        match self {
            Device::Gpu => "GPU",
            Device::Cpu => "CPU",
        }
    }
}

/// The runtime library's file name on this system.
pub const RUNTIME_LIB: &str = if cfg!(target_os = "windows") {
    "onnxruntime.dll"
} else if cfg!(target_os = "macos") {
    "libonnxruntime.dylib"
} else {
    "libonnxruntime.so"
};

static LOADED: OnceLock<Result<(), String>> = OnceLock::new();

/// Load the runtime from `dir`. Once a run: later calls give the first
/// call's outcome, whatever `dir` they name.
pub fn load(dir: &Path) -> Result<(), String> {
    LOADED
        .get_or_init(|| {
            #[cfg(target_os = "windows")]
            {
                // onnxruntime.dll asks for DirectML.dll by name, and
                // Windows would find its own, older copy in System32.
                // Loaded first by its full path, ours is the one used.
                // Kept loaded for the rest of the run.
                let dml = dir.join("DirectML.dll");
                if dml.exists() {
                    // SAFETY: a plain library with no start-up code of
                    // its own that could break the app's assumptions.
                    match unsafe { libloading::Library::new(&dml) } {
                        Ok(lib) => std::mem::forget(lib),
                        Err(e) => log::warn!("could not load {}: {e}", dml.display()),
                    }
                }
            }
            let lib = dir.join(RUNTIME_LIB);
            ort::init_from(&lib).map_err(|e| format!("could not load {}: {e}", lib.display()))?.commit();
            Ok(())
        })
        .clone()
}

/// The runtime can use the GPU here. Call after `load`.
pub fn gpu_available() -> bool {
    #[cfg(target_os = "windows")]
    {
        use ort::ep::ExecutionProvider;
        ort::ep::DirectML::default().is_available().unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

fn say<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// The vision model, ready to run.
pub struct Engine {
    session: Session,
    input: String,
    output: String,
}

impl Engine {
    /// Open `model` on `device`; on the CPU with `threads` threads. Call
    /// after `load`.
    pub fn open(model: &Path, device: Device, threads: usize) -> Result<Engine, String> {
        let mut b = Session::builder().map_err(say)?;
        match device {
            #[cfg(target_os = "windows")]
            Device::Gpu => {
                // DirectML wants one stream of work and no memory plan.
                b = b
                    .with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()])
                    .map_err(say)?
                    .with_memory_pattern(false)
                    .map_err(say)?
                    .with_parallel_execution(false)
                    .map_err(say)?;
            }
            #[cfg(not(target_os = "windows"))]
            Device::Gpu => return Err("no GPU runtime on this system".into()),
            Device::Cpu => {
                b = b
                    .with_intra_threads(threads.max(1))
                    .map_err(say)?
                    .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level1)
                    .map_err(say)?;
            }
        }
        let session = b.commit_from_file(model).map_err(say)?;
        let input = session.inputs().first().ok_or("the model takes no input")?.name().to_string();
        let outputs = session.outputs();
        let output = outputs.iter().find(|o| o.name().contains("pooler")).or(outputs.first()).ok_or("the model gives no output")?.name().to_string();
        Ok(Engine { session, input, output })
    }

    /// The vectors of `pictures` (`prep::LEN` floats each), length 1,
    /// in the same order.
    pub fn embed(&mut self, pictures: &[Vec<f32>]) -> Result<Vec<Vec<f32>>, String> {
        let n = pictures.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        let mut data = Vec::with_capacity(n * LEN);
        for p in pictures {
            if p.len() != LEN {
                return Err(format!("a prepared picture has {} floats, not {LEN}", p.len()));
            }
            data.extend_from_slice(p);
        }
        let side = SIDE as usize;
        let tensor = Tensor::from_array(([n, 3, side, side], data)).map_err(say)?;
        let out = self.session.run(ort::inputs![self.input.as_str() => tensor]).map_err(say)?;
        let (_, v) = out[self.output.as_str()].try_extract_tensor::<f32>().map_err(say)?;
        if v.len() != n * DIM {
            return Err(format!("the model gave {} numbers for {n} pictures", v.len()));
        }
        Ok(v.chunks_exact(DIM).map(unit).collect())
    }

    /// The vectors of texts as token ids (`tokenizer::LEN` each), length
    /// 1, in the same order: for the text half.
    pub fn embed_ids(&mut self, texts: &[Vec<i64>]) -> Result<Vec<Vec<f32>>, String> {
        let n = texts.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        let len = super::tokenizer::LEN;
        let data: Vec<i64> = texts.iter().flat_map(|t| t.iter().copied()).collect();
        if data.len() != n * len {
            return Err(format!("token ids must come {len} a text"));
        }
        let tensor = Tensor::from_array(([n, len], data)).map_err(say)?;
        let out = self.session.run(ort::inputs![self.input.as_str() => tensor]).map_err(say)?;
        let (_, v) = out[self.output.as_str()].try_extract_tensor::<f32>().map_err(say)?;
        if v.len() != n * DIM {
            return Err(format!("the text model gave {} numbers for {n} texts", v.len()));
        }
        Ok(v.chunks_exact(DIM).map(unit).collect())
    }
}
