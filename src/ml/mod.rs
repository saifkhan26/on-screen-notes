//! Finding pictures by what they show: a vision model (SigLIP 2, base
//! size, 32 px patches, 256 px input) turns each picture into 768
//! numbers, and pictures of the same subject — the same character, the
//! same pose, the same kind of study — get numbers that point the same
//! way (`tools::similar::by_subject`).
//!
//! * `prep` makes a picture ready for the model; `tokenizer` cuts typed
//!   words into the pieces the model's text half reads, so words can be
//!   matched with pictures too.
//! * `runtime` loads ONNX Runtime and runs the model, on the GPU through
//!   DirectML when it can, else on the CPU.
//! * `models` fetches the runtime and the model the first time picture
//!   search is turned on, into a folder of their own.
//!
//! What uses it: `canvas::picture_vectors`, which reads every picture on
//! the slides in the background and keeps their vectors.

pub mod models;
pub mod prep;
pub mod runtime;
pub mod tokenizer;
#[cfg(test)]
mod tokenizer_cases;
