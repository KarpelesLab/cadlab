//! Diagnostics: the structured messages every operation reports.
//!
//! Errors, warnings and notes share one shape so agents can act on them: a stable `code`, the
//! objects involved, an optional location and a fix `hint`.

use std::borrow::Cow;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geom::Point;
use crate::refs::ObjectRef;

/// How serious a diagnostic is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Informational.
    Info,
    /// Something is suspicious but not blocking.
    Warning,
    /// Something is wrong.
    Error,
}

/// A structured message about the design or an operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Diagnostic {
    /// Severity.
    pub severity: Severity,
    /// Stable machine-readable code, dotted: `drc.clearance`, `project.not_found`.
    pub code: Cow<'static, str>,
    /// Human-readable message.
    pub message: String,
    /// Objects concerned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subjects: Vec<ObjectRef>,
    /// Position on the board or schematic, if relevant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Point>,
    /// How to fix it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Diagnostic {
    /// New diagnostic.
    pub fn new(severity: Severity, code: impl Into<Cow<'static, str>>, message: impl Into<String>) -> Self {
        Diagnostic {
            severity,
            code: code.into(),
            message: message.into(),
            subjects: Vec::new(),
            location: None,
            hint: None,
        }
    }

    /// New error diagnostic.
    pub fn error(code: impl Into<Cow<'static, str>>, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, message)
    }

    /// New warning diagnostic.
    pub fn warning(code: impl Into<Cow<'static, str>>, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, code, message)
    }

    /// New info diagnostic.
    pub fn info(code: impl Into<Cow<'static, str>>, message: impl Into<String>) -> Self {
        Self::new(Severity::Info, code, message)
    }

    /// Adds a subject.
    pub fn with_subject(mut self, r: ObjectRef) -> Self {
        self.subjects.push(r);
        self
    }

    /// Sets the location.
    pub fn at(mut self, p: Point) -> Self {
        self.location = Some(p);
        self
    }

    /// Sets the hint.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Appends a "did you mean" hint when there are suggestions.
    pub fn with_suggestions(self, suggestions: &[String]) -> Self {
        match suggestions {
            [] => self,
            [one] => self.with_hint(format!("did you mean `{one}`?")),
            many => self.with_hint(format!(
                "did you mean one of: {}?",
                many.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ")
            )),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sev = match self.severity {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        write!(f, "{sev}[{}]: {}", self.code, self.message)?;
        if !self.subjects.is_empty() {
            let s: Vec<String> = self.subjects.iter().map(ToString::to_string).collect();
            write!(f, " ({})", s.join(", "))?;
        }
        if let Some(p) = self.location {
            write!(f, " at ({}, {})", p.x, p.y)?;
        }
        if let Some(h) = &self.hint {
            write!(f, "\n  hint: {h}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_json() {
        let d = Diagnostic::error("bom.part_not_found", "no part `R1O`")
            .with_subject(ObjectRef::Name("R1O".into()))
            .with_suggestions(&["R10".into()]);
        assert_eq!(
            d.to_string(),
            "error[bom.part_not_found]: no part `R1O` (R1O)\n  hint: did you mean `R10`?"
        );
        let j = serde_json::to_value(&d).unwrap();
        assert_eq!(j["severity"], "error");
        assert!(j.get("location").is_none());
        let back: Diagnostic = serde_json::from_value(j).unwrap();
        assert_eq!(back, d);
    }
}
