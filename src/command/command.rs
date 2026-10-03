use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{CommandError, Context};

/// How a command interacts with project state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    /// Reads only. Never changes the project.
    Query,
    /// Changes the project inside a transaction: rolled back on error or dry-run, recorded in
    /// undo history and the operation log on success.
    Mutation,
    /// Manages the session itself (create/open/save a project, undo/redo). Not transactional,
    /// not allowed in batches.
    Session,
}

/// An operation. Every user-facing action in cadlab is one of these.
///
/// The struct's fields are the arguments. Its JSON Schema drives CLI flags and MCP tool
/// schemas, so field docs are user-facing help text.
pub trait Command: Serialize + DeserializeOwned + JsonSchema {
    /// Stable dotted name, `<group>.<action>`: `project.new`, `bom.add`.
    const NAME: &'static str;
    /// One-line description for help and tool listings.
    const SUMMARY: &'static str;
    /// State interaction.
    const KIND: CommandKind;
    /// Arguments taken positionally on the CLI, in order.
    const POSITIONAL: &'static [&'static str] = &[];

    /// Result type.
    type Output: Serialize + JsonSchema;

    /// Runs the command.
    fn run(self, ctx: &mut Context<'_>) -> Result<Self::Output, CommandError>;

    /// Short human-readable summary of a result, for the CLI and MCP text content.
    fn summarize(output: &Self::Output) -> String {
        serde_json::to_string(output).unwrap_or_default()
    }
}
