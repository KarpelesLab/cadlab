use crate::Diagnostic;
use indexmap::IndexMap;
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{CancelToken, Command, CommandError, CommandKind, Context, NoProgress, Progress, Session};

/// A command invocation in data form: one line of a batch file or of the operation log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    /// Command name, e.g. `project.set`.
    pub cmd: String,
    /// Arguments object.
    #[serde(default, skip_serializing_if = "is_empty_args")]
    pub args: Value,
}

fn is_empty_args(v: &Value) -> bool {
    v.is_null() || v.as_object().is_some_and(|o| o.is_empty())
}

/// Options for running commands.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunOptions {
    /// Run, collect the result, then roll back.
    pub dry_run: bool,
}

/// A successful command run.
#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    /// Command name.
    pub command: String,
    /// Structured result.
    pub output: Value,
    /// Short text summary of the result.
    pub summary: String,
    /// Warnings and notes.
    pub diagnostics: Vec<Diagnostic>,
    /// Whether session state changed (and should be saved).
    pub changed: bool,
}

/// A failed command run. State is unchanged.
#[derive(Clone, Debug, Serialize)]
pub struct Failure {
    /// Command name (or `batch`).
    pub command: String,
    /// For batches, the index of the failing step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<usize>,
    /// The error.
    pub error: CommandError,
    /// Diagnostics reported before failing.
    pub diagnostics: Vec<Diagnostic>,
}

type RunFn = fn(&mut Context<'_>, Value) -> Result<(Value, String), CommandError>;

/// Type-erased registry entry.
pub struct Entry {
    /// Dotted name.
    pub name: &'static str,
    /// One-line description.
    pub summary: &'static str,
    /// State interaction.
    pub kind: CommandKind,
    /// CLI positional arguments.
    pub positional: &'static [&'static str],
    /// JSON Schema of the arguments.
    pub input_schema: Value,
    /// JSON Schema of the output.
    pub output_schema: Value,
    run: RunFn,
}

impl Entry {
    /// Group: the part before the last dot (`project` for `project.new`).
    pub fn group(&self) -> &'static str {
        self.name.rsplit_once('.').map_or(self.name, |(g, _)| g)
    }

    /// Action: the part after the last dot (`new` for `project.new`).
    pub fn action(&self) -> &'static str {
        self.name.rsplit_once('.').map_or(self.name, |(_, a)| a)
    }

    /// Full description, as returned by `describe`.
    pub fn describe(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "summary": self.summary,
            "kind": self.kind,
            "positional": self.positional,
            "input_schema": self.input_schema,
            "output_schema": self.output_schema,
        })
    }
}

/// All commands, by name. CLI subcommands and MCP tools are generated from this.
#[derive(Default)]
pub struct Registry {
    entries: IndexMap<&'static str, Entry>,
}

/// Generates the JSON Schema of `T`, self-contained (no `$ref`s) so it can be embedded in tool
/// definitions as is.
pub fn schema_of<T: JsonSchema>() -> Value {
    let mut settings = SchemaSettings::draft2020_12().with(|s| {
        s.inline_subschemas = true;
        s.meta_schema = None;
    });
    settings = settings.for_deserialize();
    let mut v = settings.into_generator().into_root_schema_for::<T>().to_value();
    if let Some(o) = v.as_object_mut() {
        o.remove("title");
    }
    v
}

fn run_erased<C: Command>(ctx: &mut Context<'_>, args: Value) -> Result<(Value, String), CommandError> {
    let args = if args.is_null() { Value::Object(Default::default()) } else { args };
    let cmd: C = serde_json::from_value(args).map_err(|e| {
        CommandError::invalid_args("args.invalid", format!("invalid arguments for `{}`: {e}", C::NAME))
            .with_hint(format!("see `describe {}` for the expected arguments", C::NAME))
    })?;
    let out = cmd.run(ctx)?;
    let summary = C::summarize(&out);
    let value = serde_json::to_value(&out)
        .map_err(|e| CommandError::new(super::ErrorKind::Internal, "internal.serialize", e.to_string()))?;
    Ok((value, summary))
}

impl Registry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry with every built-in command.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        crate::commands::register_all(&mut r);
        r
    }

    /// Adds a command.
    ///
    /// # Panics
    /// If a command with the same name is already registered.
    pub fn register<C: Command>(&mut self) -> &mut Self {
        let e = Entry {
            name: C::NAME,
            summary: C::SUMMARY,
            kind: C::KIND,
            positional: C::POSITIONAL,
            input_schema: schema_of::<C>(),
            output_schema: schema_of::<C::Output>(),
            run: run_erased::<C>,
        };
        assert!(self.entries.insert(C::NAME, e).is_none(), "duplicate command {}", C::NAME);
        self
    }

    /// Looks up a command.
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.get(name)
    }

    /// All commands, in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    /// Groups, in registration order, each with its commands.
    pub fn groups(&self) -> IndexMap<&'static str, Vec<&Entry>> {
        let mut g: IndexMap<&'static str, Vec<&Entry>> = IndexMap::new();
        for e in self.entries.values() {
            g.entry(e.group()).or_default().push(e);
        }
        g
    }

    fn lookup(&self, name: &str) -> Result<&Entry, CommandError> {
        self.get(name).ok_or_else(|| {
            let s = crate::suggest::did_you_mean(name, self.entries.keys().copied(), 3);
            CommandError::not_found("command.unknown", format!("unknown command `{name}`")).with_suggestions(&s)
        })
    }

    /// Runs a command by name with JSON arguments.
    pub fn execute(
        &self,
        session: &mut Session,
        name: &str,
        args: Value,
        opts: RunOptions,
    ) -> Result<Outcome, Failure> {
        self.execute_with(session, name, args, opts, &NoProgress, &CancelToken::new())
    }

    /// Like [`Registry::execute`], with progress reporting and cancellation.
    pub fn execute_with(
        &self,
        session: &mut Session,
        name: &str,
        args: Value,
        opts: RunOptions,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<Outcome, Failure> {
        let fail = |error, diagnostics| Failure { command: name.to_string(), step: None, error, diagnostics };
        let entry = self.lookup(name).map_err(|e| fail(e, vec![]))?;
        let step = Step { cmd: name.to_string(), args: args.clone() };
        let run = entry.run;
        transact(session, entry.kind, name, vec![step], opts, progress, cancel, |ctx| run(ctx, args))
            .map(|(output, summary, diagnostics, changed)| Outcome {
                command: name.to_string(),
                output,
                summary,
                diagnostics,
                changed,
            })
            .map_err(|(e, d)| fail(e, d))
    }

    /// Runs several commands as one transaction: all succeed, or nothing changes. Session
    /// commands are not allowed. Produces a single undo step.
    pub fn execute_batch(
        &self,
        session: &mut Session,
        steps: Vec<Step>,
        opts: RunOptions,
    ) -> Result<Vec<Outcome>, Failure> {
        let mut entries = Vec::with_capacity(steps.len());
        for (i, s) in steps.iter().enumerate() {
            let fail = |error| Failure { command: "batch".into(), step: Some(i), error, diagnostics: vec![] };
            let e = self.lookup(&s.cmd).map_err(fail)?;
            if e.kind == CommandKind::Session {
                return Err(fail(
                    CommandError::invalid_args(
                        "batch.session_command",
                        format!("`{}` manages the session and cannot run inside a batch", s.cmd),
                    )
                    .with_hint("run it separately, before or after the batch"),
                ));
            }
            entries.push(e);
        }
        let mutating = entries.iter().any(|e| e.kind == CommandKind::Mutation);
        let kind = if mutating { CommandKind::Mutation } else { CommandKind::Query };
        let label = format!("batch ({} commands)", steps.len());
        let mut failed_at = None;
        let logged: Vec<Step> = steps
            .iter()
            .zip(&entries)
            .filter(|(_, e)| e.kind == CommandKind::Mutation)
            .map(|(s, _)| s.clone())
            .collect();
        let res = transact(session, kind, &label, logged, opts, &NoProgress, &CancelToken::new(), |ctx| {
            let mut outs = Vec::with_capacity(steps.len());
            for (i, (s, e)) in steps.iter().zip(&entries).enumerate() {
                let before = ctx.diagnostics.len();
                match (e.run)(ctx, s.args.clone()) {
                    Ok((output, summary)) => {
                        let diagnostics = ctx.diagnostics[before..].to_vec();
                        outs.push(Outcome { command: s.cmd.clone(), output, summary, diagnostics, changed: false });
                    }
                    Err(err) => {
                        failed_at = Some(i);
                        return Err(err);
                    }
                }
            }
            Ok((outs, String::new()))
        });
        match res {
            Ok((mut outs, _, _, changed)) => {
                for o in &mut outs {
                    o.changed = changed;
                }
                Ok(outs)
            }
            Err((error, diagnostics)) => Err(Failure { command: "batch".into(), step: failed_at, error, diagnostics }),
        }
    }
}

type TransactResult<T> = Result<(T, String, Vec<Diagnostic>, bool), (CommandError, Vec<Diagnostic>)>;

/// Runs `f` with transaction semantics according to `kind`.
#[allow(clippy::too_many_arguments)]
fn transact<T>(
    session: &mut Session,
    kind: CommandKind,
    label: &str,
    log: Vec<Step>,
    opts: RunOptions,
    progress: &dyn Progress,
    cancel: &CancelToken,
    f: impl FnOnce(&mut Context<'_>) -> Result<(T, String), CommandError>,
) -> TransactResult<T> {
    match kind {
        CommandKind::Query => {
            let mut ctx = Context::new(session, progress, cancel, opts.dry_run);
            let r = f(&mut ctx);
            let d = std::mem::take(&mut ctx.diagnostics);
            r.map(|(v, s)| (v, s, d.clone(), false)).map_err(|e| (e, d))
        }
        CommandKind::Session => {
            if opts.dry_run {
                return Err((
                    CommandError::invalid_args("dry_run.unsupported", format!("`{label}` cannot be dry-run")),
                    vec![],
                ));
            }
            let changes_before = session.change_count();
            let mut ctx = Context::new(session, progress, cancel, opts.dry_run);
            let r = f(&mut ctx);
            let d = std::mem::take(&mut ctx.diagnostics);
            match r {
                Ok((v, s)) => {
                    let changed = session.change_count() != changes_before;
                    if changed {
                        session.log(log);
                        // The command saved by itself (e.g. project.new): write its log entry now.
                        if !session.is_dirty()
                            && let Err(e) = session.flush_log()
                        {
                            return Err((e, d));
                        }
                    }
                    Ok((v, s, d, changed))
                }
                Err(e) => Err((e, d)),
            }
        }
        CommandKind::Mutation => {
            let Some(before) = session.project.clone() else {
                let ctx = Context::new(session, progress, cancel, opts.dry_run);
                let e = ctx.project().map(|_| ()).unwrap_err();
                return Err((e, vec![]));
            };
            let mut ctx = Context::new(session, progress, cancel, opts.dry_run);
            let r = f(&mut ctx);
            let d = std::mem::take(&mut ctx.diagnostics);
            match r {
                Ok((v, s)) if opts.dry_run => {
                    session.project = Some(before);
                    Ok((v, s, d, false))
                }
                Ok((v, s)) => {
                    let changed = session.project.as_ref() != Some(&before);
                    if changed {
                        session.history.record(label.to_string(), before);
                        session.log(log);
                        session.mark_dirty();
                    }
                    Ok((v, s, d, changed))
                }
                Err(e) => {
                    session.project = Some(before);
                    Err((e, d))
                }
            }
        }
    }
}

/// Typed entry point for Rust callers: runs `cmd` with the same transaction semantics as
/// [`Registry::execute`].
pub fn run<C: Command>(
    session: &mut Session,
    cmd: C,
    opts: RunOptions,
) -> Result<(C::Output, Vec<Diagnostic>), CommandError> {
    let step = Step { cmd: C::NAME.to_string(), args: serde_json::to_value(&cmd).unwrap_or(Value::Null) };
    transact(session, C::KIND, C::NAME, vec![step], opts, &NoProgress, &CancelToken::new(), |ctx| {
        cmd.run(ctx).map(|o| (o, String::new()))
    })
    .map(|(o, _, d, _)| (o, d))
    .map_err(|(e, _)| e)
}
