//! `render.board`: layer view, realistic top/bottom view, highlight, ratsnest, markers, crop.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Rendered, out_path, summary, write_scene};
use crate::command::{Command, CommandError, CommandKind, Context};
use crate::geom::Point;
use crate::model::board::BoardSide;
use crate::render::View;
use crate::render::board::{self as draw, BoardView, Marker};
use crate::suggest::did_you_mean;
use crate::units::{LengthUnit, Nm};

/// Side for the realistic view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RealisticSide {
    /// Top side, as seen from above.
    Top,
    /// Bottom side, as seen from below (mirrored).
    Bottom,
}

/// A marker drawn on the board image (e.g. a DRC violation).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkerArg {
    /// Position: ["12mm", "4.5mm"].
    pub at: Point,
    /// Label drawn next to the marker.
    #[serde(default)]
    pub label: String,
}

/// Render the board: copper layers, silkscreen, outline, holes, ratsnest; or a realistic view.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Board {
    /// Output file, .png or .svg (default out/board.png, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Layers to draw: copper (`F.Cu`, `In1.Cu`, `B.Cu`), `F.SilkS`/`B.SilkS`, `F.Fab`/`B.Fab`,
    /// `F.CrtYd`/`B.CrtYd`, `F.Mask`/`B.Mask`, `F.Paste`/`B.Paste`, `Edge.Cuts`, `User.*`.
    /// Default: all copper, both silkscreens and the outline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layers: Option<Vec<String>>,
    /// Draw unrouted connections as thin lines (default true; not drawn in the realistic view).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratsnest: Option<bool>,
    /// Nets or designators to emphasize; everything else is dimmed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub highlight: Vec<String>,
    /// Realistic view as manufactured: solder mask, silkscreen, exposed pad finish (colors from
    /// `board.setup` preferences). The bottom view is mirrored, as seen from below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realistic: Option<RealisticSide>,
    /// Crop to a component's courtyard plus `margin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub around: Option<String>,
    /// Margin around the cropped component (default 3mm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin: Option<Nm>,
    /// Markers to draw (e.g. DRC violations): [{"at": ["5mm", "3mm"], "label": "clearance"}].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<MarkerArg>,
    /// PNG resolution in pixels per millimeter (default: the longer side is about 1600 px).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub px_per_mm: Option<f64>,
}

impl Command for Board {
    const NAME: &'static str = "render.board";
    const SUMMARY: &'static str =
        "Render the board (layers, holes, ratsnest, highlight, markers) or a realistic top/bottom view";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = Rendered;

    fn run(self, ctx: &mut Context<'_>) -> Result<Rendered, CommandError> {
        let p = ctx.project()?;
        let board = p.board();
        if let Some(layers) = &self.layers {
            let known = draw::known_layers(p);
            for l in layers {
                if !known.contains(l) {
                    return Err(CommandError::invalid_args("render.unknown_layer", format!("unknown layer `{l}`"))
                        .with_suggestions(&did_you_mean(l, known.iter().map(String::as_str), 3))
                        .with_hint(format!("layers of this board: {}", known.join(", "))));
                }
            }
        }
        let nets: Vec<&str> = p.circuit().nets.keys().map(String::as_str).collect();
        let mut highlight = Vec::new();
        for h in &self.highlight {
            if board.footprints.contains_key(h) || nets.contains(&h.as_str()) {
                highlight.push(h.clone());
            } else {
                let cands = nets.iter().copied().chain(board.footprints.keys().map(String::as_str));
                return Err(CommandError::not_found(
                    "render.unknown_highlight",
                    format!("`{h}` is neither a net nor a placed component"),
                )
                .with_suggestions(&did_you_mean(h, cands, 3))
                .with_hint("highlight takes net names (GND) or designators of placed components (U1)"));
            }
        }
        let side = self.realistic.map(|r| match r {
            RealisticSide::Top => BoardSide::Top,
            RealisticSide::Bottom => BoardSide::Bottom,
        });
        let area = match &self.around {
            Some(r) => {
                let a = draw::footprint_area(p, r).ok_or_else(|| {
                    CommandError::not_found("render.not_placed", format!("`{r}` is not placed on the board"))
                        .with_suggestions(&did_you_mean(r, board.footprints.keys().map(String::as_str), 3))
                        .with_hint("place it first (`place set`), or pick a placed component")
                })?;
                let m = self.margin.unwrap_or(Nm::from_um(3000)).to_f64(LengthUnit::Mm).max(0.0);
                (a.0 - m, a.1 - m, a.2 + m, a.3 + m)
            }
            None => {
                let a = draw::extent(p).ok_or_else(|| {
                    CommandError::invalid_args("render.empty_board", "the board has no outline and nothing placed")
                        .with_hint("define an outline (`board outline`) or place components first")
                })?;
                // Markers off the board stay in frame.
                let a = self.markers.iter().fold(a, |a, m| {
                    let (x, y) = (m.at.x.to_f64(LengthUnit::Mm), m.at.y.to_f64(LengthUnit::Mm));
                    (a.0.min(x), a.1.min(y), a.2.max(x), a.3.max(y))
                });
                (a.0 - 1.0, a.1 - 1.0, a.2 + 1.0, a.3 + 1.0)
            }
        };
        let area = if side == Some(BoardSide::Bottom) { draw::mirror_area(area) } else { area };
        let longest = (area.2 - area.0).max(area.3 - area.1).max(1.0);
        let px_per_mm = match self.px_per_mm {
            Some(v) => v.clamp(1.0, 400.0),
            None => (1600.0 / longest).clamp(4.0, 200.0),
        };
        let view = BoardView {
            layers: self.layers.clone(),
            ratsnest: self.ratsnest.unwrap_or(true),
            highlight,
            realistic: side,
            markers: self.markers.iter().map(|m| Marker { at: m.at, label: m.label.clone() }).collect(),
            px_per_mm,
        };
        let scene = draw::draw(p, &view);
        let view = View { area, px_per_mm };
        let path = out_path(ctx, &self.path, "board.png");
        write_scene(&scene, &view, &path)
    }

    fn summarize(o: &Rendered) -> String {
        summary(o)
    }
}
