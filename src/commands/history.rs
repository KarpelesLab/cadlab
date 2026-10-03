//! `history.*`: undo, redo and list steps.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandError, CommandKind, Context, HistoryItem, Registry};

pub(crate) fn register(r: &mut Registry) {
    r.register::<Undo>().register::<Redo>().register::<List>();
}

/// Result of undo/redo.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct StepResult {
    /// Steps undone or redone, in order.
    pub steps: Vec<String>,
    /// Remaining undo steps.
    pub undo_depth: usize,
    /// Remaining redo steps.
    pub redo_depth: usize,
}

fn default_steps() -> u32 {
    1
}

/// Undo the last change(s).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Undo {
    /// Number of steps to undo.
    #[serde(default = "default_steps")]
    pub steps: u32,
}

/// Redo undone change(s).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Redo {
    /// Number of steps to redo.
    #[serde(default = "default_steps")]
    pub steps: u32,
}

fn step(ctx: &mut Context<'_>, steps: u32, forward: bool) -> Result<StepResult, CommandError> {
    let mut done = Vec::new();
    for _ in 0..steps.max(1) {
        let current = ctx.project()?.clone();
        let r = if forward {
            ctx.session.history.redo(current)
        } else {
            ctx.session.history.undo(current)
        };
        match r? {
            Some((label, project)) => {
                ctx.session.project = Some(project);
                ctx.session.mark_dirty();
                done.push(label);
            }
            None => break,
        }
    }
    if done.is_empty() {
        let what = if forward { "redo" } else { "undo" };
        return Err(CommandError::conflict(
            "history.empty",
            format!("nothing to {what}"),
        ));
    }
    Ok(StepResult {
        steps: done,
        undo_depth: ctx.session.history.undo_len(),
        redo_depth: ctx.session.history.redo_len(),
    })
}

fn text(verb: &str, o: &StepResult) -> String {
    format!(
        "{verb}: {} (undo: {}, redo: {})",
        o.steps.join(", "),
        o.undo_depth,
        o.redo_depth
    )
}

impl Command for Undo {
    const NAME: &'static str = "history.undo";
    const SUMMARY: &'static str = "Undo the last change(s)";
    const KIND: CommandKind = CommandKind::Session;
    type Output = StepResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<StepResult, CommandError> {
        step(ctx, self.steps, false)
    }

    fn summarize(o: &StepResult) -> String {
        text("undone", o)
    }
}

impl Command for Redo {
    const NAME: &'static str = "history.redo";
    const SUMMARY: &'static str = "Redo undone change(s)";
    const KIND: CommandKind = CommandKind::Session;
    type Output = StepResult;

    fn run(self, ctx: &mut Context<'_>) -> Result<StepResult, CommandError> {
        step(ctx, self.steps, true)
    }

    fn summarize(o: &StepResult) -> String {
        text("redone", o)
    }
}

/// List undo and redo steps.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {}

/// Result of `history.list`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HistoryList {
    /// Undoable steps, oldest first.
    pub undo: Vec<HistoryItem>,
    /// Redoable steps, next one last.
    pub redo: Vec<HistoryItem>,
}

impl Command for List {
    const NAME: &'static str = "history.list";
    const SUMMARY: &'static str = "List undo and redo steps";
    const KIND: CommandKind = CommandKind::Query;
    type Output = HistoryList;

    fn run(self, ctx: &mut Context<'_>) -> Result<HistoryList, CommandError> {
        ctx.project()?;
        Ok(HistoryList {
            undo: ctx.session.history.undo_items(),
            redo: ctx.session.history.redo_items(),
        })
    }

    fn summarize(o: &HistoryList) -> String {
        if o.undo.is_empty() && o.redo.is_empty() {
            return "no history".into();
        }
        let mut s = String::new();
        for (i, e) in o.undo.iter().enumerate() {
            s += &format!("{:>4}  {}\n", i + 1, e.label);
        }
        for e in o.redo.iter().rev() {
            s += &format!("redo  {}\n", e.label);
        }
        s.trim_end().to_string()
    }
}
