//! The command system.
//!
//! Every operation is a [`Command`]: a serializable struct with a JSON Schema and a typed
//! output. The [`Registry`] holds them all; the CLI and the MCP server are generated from it,
//! and Rust callers use [`run`] or the registry directly. See `docs/ARCHITECTURE.md`.
//!
//! Transactions: a [`CommandKind::Mutation`] runs against a snapshot of the project. On error
//! or dry-run the snapshot is restored; on success it becomes an undo step.

#[allow(clippy::module_inception)]
mod command;
mod context;
mod error;
mod history;
mod registry;
mod session;

pub use command::{Command, CommandKind};
pub use context::{CancelToken, Context, NoProgress, Progress};
pub use error::{CommandError, ErrorKind};
pub use history::{HISTORY_LIMIT, History, HistoryItem};
pub use registry::{Entry, Failure, Outcome, Registry, RunOptions, Step, run, schema_of};
pub use session::{CACHE_DIR, Session};
