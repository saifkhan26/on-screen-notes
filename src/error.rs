//! Error type and `Result` alias used throughout the crate.
//!
//! We use the `anyhow` crate because it gives us:
//!   * a single `anyhow::Error` type that can wrap any other error,
//!   * an `?`-friendly chain of context messages,
//!   * a clean `Result<T>` type so we don't have to write `Result<T, MyError>`
//!     in every function signature.
//!
//! Beginners: `Result<T>` here is `anyhow::Result<T>`. When you see `?` after
//! a fallible call, it means "if this returned an `Err`, return it from the
//! current function; otherwise unwrap the `Ok` value."

/// Project-wide `Result` alias. Every function that can fail returns this.
pub type Result<T> = anyhow::Result<T>;

/// Re-export `anyhow::Error` so callers don't need to depend on `anyhow`
/// themselves. They can write `crate::error::Error`.
#[allow(dead_code)]
pub type Error = anyhow::Error;

/// Re-export `anyhow!` and `bail!` macros for convenience. Inside other
/// modules use them as `crate::error::bail!("…")`.
pub use anyhow::{anyhow, Context};
// `bail!` re-exported behind `#[allow(unused_imports)]` because it's commonly
// useful as the codebase grows. Currently not used — keeping it imported
// makes refactoring smoother.
#[allow(unused_imports)]
pub use anyhow::bail;
