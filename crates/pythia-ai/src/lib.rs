//! The AI assistant's model side: what is sent and how.
//!
//! - [`prompt`]: context parts, the data policy and the instructions per task.
//!   Every request is built there, so the policy holds for all of them.
//! - [`client`]: blocking calls to the Anthropic Messages API, OpenAI-compatible
//!   chat completions or a command line tool. Callers run them on a worker
//!   thread.
//! - [`answer`]: splitting an answer into prose and code blocks.

pub mod answer;
pub mod client;
pub mod prompt;

pub use client::{ChatRequest, Error, Message, Role, complete};
pub use prompt::{Part, Policy, Sensitivity, Task, Turn};
