//! `silk.*`: reference designators on the silkscreen (docs/BOARD.md, "Silkscreen").

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util;
use crate::board::{self as geo, silk};
use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::diag::Diagnostic;
use crate::geom::Point;
use crate::model::Project;
use crate::model::sections::natural_cmp;
use crate::refs::ObjectRef;
use crate::units::{Angle, Nm};

pub(crate) fn register(r: &mut Registry) {
    r.register::<Arrange>().register::<Set>().register::<Reset>().register::<List>();
}

/// How a reference designator's position was decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TextOrigin {
    /// Centered above the courtyard (never arranged, or free there).
    Default,
    /// Moved by `silk.arrange` (or `place.auto`).
    Arranged,
    /// Set by hand (`silk.set`) or imported: never moved.
    Manual,
}

/// A reference designator on the silkscreen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TextInfo {
    /// Text center on the board.
    pub at: Point,
    /// Counter-clockwise rotation.
    pub rotation: Angle,
    /// Cap height.
    pub size: Nm,
    /// Stroke width.
    pub stroke: Nm,
    /// Side it is printed on (`F.SilkS` or `B.SilkS`).
    pub layer: String,
    /// Not printed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// How the position was decided.
    pub origin: TextOrigin,
}

fn text_info(p: &Project, refdes: &str) -> Option<TextInfo> {
    let pf = p.board().footprints.get(refdes)?;
    let t = silk::ref_text_any(p, refdes)?;
    let stored = pf.ref_text.as_ref();
    Some(TextInfo {
        at: t.at,
        rotation: t.rotation,
        size: t.size,
        stroke: t.width,
        layer: geo::side_layer(pf.side, "F.SilkS"),
        hidden: stored.is_some_and(|s| s.hidden),
        origin: match stored {
            Some(s) if s.manual => TextOrigin::Manual,
            Some(_) => TextOrigin::Arranged,
            None => TextOrigin::Default,
        },
    })
}

fn info_line(r: &str, t: &TextInfo) -> String {
    let how = match t.origin {
        TextOrigin::Default => "default",
        TextOrigin::Arranged => "arranged",
        TextOrigin::Manual => "manual",
    };
    let hidden = if t.hidden { ", hidden" } else { "" };
    format!(
        "{r}: ({}, {}) {}° {} on {} ({how}{hidden})",
        t.at.x,
        t.at.y,
        crate::fabout::deg(t.rotation),
        t.size,
        t.layer
    )
}

/// The placed component `refdes` names, with a footprint.
fn placed(p: &Project, refdes: &str) -> Result<String, CommandError> {
    let r = util::refdes_key(p, refdes)?;
    if !p.board().footprints.contains_key(&r) {
        return Err(CommandError::conflict("place.not_placed", format!("{r} is not placed"))
            .with_hint(format!("place it first: `place.set {r} --at ...`")));
    }
    if geo::footprint_for(p, &r).is_none() {
        return Err(CommandError::invalid_args("place.no_footprint", format!("{r} has no footprint"))
            .with_hint("give its part a footprint (`part.set --footprint` or `footprint.generate`)"));
    }
    Ok(r)
}

/// Runs [`silk::arrange`] and applies it: used by `silk.arrange` and after `place.auto`. Texts
/// that found no free position are hidden and reported in one warning.
pub(super) fn arrange_texts(
    ctx: &mut Context<'_>,
    only: Option<&BTreeSet<String>>,
) -> Result<silk::Arrangement, CommandError> {
    let a = silk::arrange(ctx.project()?, only);
    if !a.moved.is_empty() {
        let board = ctx.project_mut()?.board_mut();
        for r in &a.moved {
            if let Some(pf) = board.footprints.get_mut(r) {
                pf.ref_text = a.texts.get(r).cloned().flatten();
            }
        }
    }
    if !a.hidden.is_empty() {
        let mut d = Diagnostic::warning(
            "silk.hidden",
            format!(
                "no free position on the silkscreen for the reference designator of {}: left out of the legend",
                a.hidden.join(", ")
            ),
        )
        .with_hint(
            "give the parts more room and run `silk.arrange` again, or set a position by hand (`silk.set <refdes> \
             --at ...`, a smaller `--size`); `silk.list` shows every text",
        );
        for r in &a.hidden {
            d = d.with_subject(ObjectRef::Name(r.clone()));
        }
        ctx.report(d);
    }
    Ok(a)
}

/// Move reference designators to free positions on the silkscreen: around each footprint
/// (above, below, left, right, turned a quarter beside it, slid along its sides, then up to
/// 1 mm farther) and, for large parts, inside the courtyard. A free position is on the board
/// away from its edges, off pads and solder mask openings, off every other silkscreen drawing
/// and text, and on no other part's courtyard. A text without one is hidden, with a warning.
/// Texts set with `silk.set` are never moved. `place.auto` runs this by itself; run it again
/// after moving parts by hand (`drc.run` reports texts that are clipped or overlap).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Arrange {
    /// Only these designators (default: every placed footprint); the other texts stay where
    /// they are and are kept clear of.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refdes: Vec<String>,
}

/// What `silk.arrange` did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Arranged {
    /// Texts arranged (not counting those set by hand).
    pub texts: usize,
    /// Texts whose position, rotation or visibility changed, with where they are now.
    pub moved: BTreeMap<String, TextInfo>,
    /// Texts hidden for lack of a free position.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    /// Texts set by hand, left alone.
    pub manual: usize,
}

impl Command for Arrange {
    const NAME: &'static str = "silk.arrange";
    const SUMMARY: &'static str = "Move reference designators to free positions (those set by hand stay)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Arranged;

    fn run(self, ctx: &mut Context<'_>) -> Result<Arranged, CommandError> {
        let mut only = BTreeSet::new();
        for r in &self.refdes {
            only.insert(placed(ctx.project()?, r)?);
        }
        let a = arrange_texts(ctx, (!only.is_empty()).then_some(&only))?;
        let p = ctx.project()?;
        let moved = a.moved.iter().filter_map(|r| text_info(p, r).map(|t| (r.clone(), t))).collect();
        Ok(Arranged { texts: a.texts.len(), moved, hidden: a.hidden, manual: a.manual })
    }

    fn summarize(o: &Arranged) -> String {
        let mut lines = vec![format!(
            "{} reference designators arranged: {} moved, {} hidden, {} set by hand left alone",
            o.texts,
            o.moved.len() - o.moved.values().filter(|t| t.hidden).count(),
            o.hidden.len(),
            o.manual
        )];
        let mut keys: Vec<&String> = o.moved.keys().collect();
        keys.sort_by(|a, b| natural_cmp(a, b));
        lines.extend(keys.into_iter().map(|r| info_line(r, &o.moved[r])));
        lines.join("\n")
    }
}

/// Set a reference designator's position, rotation, size or visibility by hand. A text set
/// this way is never moved by `silk.arrange` or `place.auto`; it follows its part when the
/// part moves. `silk.reset` hands it back.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Set {
    /// Designator.
    pub refdes: String,
    /// Text center on the board: ["12mm", "8mm"] (on the CLI: `--at 12mm,8mm`). Unset: where
    /// the text is now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Point>,
    /// Rotation, degrees counter-clockwise (0 reads left to right, 90 bottom to top).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Angle>,
    /// Cap height (default 1mm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Nm>,
    /// Stroke width (default: 15 % of the height, at least the rules' `min_silk_width`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<Nm>,
    /// Leave the text out of the legend (`false` prints it again).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
}

/// Reference designator texts by designator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Texts {
    /// The texts.
    pub texts: BTreeMap<String, TextInfo>,
}

fn texts_text(o: &Texts) -> String {
    if o.texts.is_empty() {
        return "no reference designators (nothing is placed)".into();
    }
    let mut keys: Vec<&String> = o.texts.keys().collect();
    keys.sort_by(|a, b| natural_cmp(a, b));
    keys.into_iter().map(|r| info_line(r, &o.texts[r])).collect::<Vec<_>>().join("\n")
}

impl Command for Set {
    const NAME: &'static str = "silk.set";
    const SUMMARY: &'static str = "Set a designator's position, rotation, size, visibility by hand";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Texts;

    fn run(self, ctx: &mut Context<'_>) -> Result<Texts, CommandError> {
        let r = placed(ctx.project()?, &self.refdes)?;
        if let Some(s) = self.size.filter(|s| s.0 <= 0) {
            return Err(CommandError::invalid_args("silk.invalid_size", format!("a text cannot be {s} high"))
                .with_hint("give a cap height such as `--size 1mm` (fabs print 0.8 to 1 mm reliably)"));
        }
        if let Some(w) = self.stroke.filter(|w| w.0 <= 0) {
            return Err(CommandError::invalid_args("silk.invalid_size", format!("a stroke cannot be {w} wide"))
                .with_hint("give a stroke width such as `--stroke 0.15mm` (about 15 % of the height reads well)"));
        }
        let p = ctx.project()?;
        // What is not given stays: a text above its courtyard keeps following the courtyard.
        let pf = &p.board().footprints[&r];
        let mut t = pf.ref_text.clone().unwrap_or_default();
        if let Some(at) = self.at {
            t.offset = Some(at - pf.at);
        }
        if let Some(a) = self.rotation {
            t.rotation = a.normalized();
        }
        if let Some(s) = self.size {
            t.size = Some(s);
        }
        if let Some(w) = self.stroke {
            t.thickness = Some(w);
        }
        if let Some(h) = self.hidden {
            t.hidden = h;
        }
        t.manual = true;
        ctx.project_mut()?.board_mut().footprints.get_mut(&r).expect("placed").ref_text = Some(t);
        let p = ctx.project()?;
        Ok(Texts { texts: text_info(p, &r).map(|t| (r, t)).into_iter().collect() })
    }

    fn summarize(o: &Texts) -> String {
        texts_text(o)
    }
}

/// Hand reference designators back to cadlab: the position, rotation, size and visibility set
/// with `silk.set` (or imported) are dropped, the text returns above its courtyard and
/// `silk.arrange` may move it again.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reset {
    /// Designators.
    pub refdes: Vec<String>,
}

impl Command for Reset {
    const NAME: &'static str = "silk.reset";
    const SUMMARY: &'static str = "Drop what `set` did to designators";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Texts;

    fn run(self, ctx: &mut Context<'_>) -> Result<Texts, CommandError> {
        let mut keys = Vec::new();
        for r in &self.refdes {
            keys.push(placed(ctx.project()?, r)?);
        }
        let board = ctx.project_mut()?.board_mut();
        for r in &keys {
            board.footprints.get_mut(r).expect("placed").ref_text = None;
        }
        let p = ctx.project()?;
        Ok(Texts { texts: keys.into_iter().filter_map(|r| text_info(p, &r).map(|t| (r, t))).collect() })
    }

    fn summarize(o: &Texts) -> String {
        texts_text(o)
    }
}

/// List the reference designator of every placed footprint: where the legend prints it, its
/// rotation and size, whether it is hidden, and whether the position is the default, arranged
/// or set by hand.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct List {}

impl Command for List {
    const NAME: &'static str = "silk.list";
    const SUMMARY: &'static str = "List designators as printed";
    const KIND: CommandKind = CommandKind::Query;
    type Output = Texts;

    fn run(self, ctx: &mut Context<'_>) -> Result<Texts, CommandError> {
        let p = ctx.project()?;
        Ok(Texts {
            texts: p.board().footprints.keys().filter_map(|r| text_info(p, r).map(|t| (r.clone(), t))).collect(),
        })
    }

    fn summarize(o: &Texts) -> String {
        texts_text(o)
    }
}
