//! `render.board3d`: isometric (or any orthographic angle) 3D view of the assembled board.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::board::RealisticSide;
use super::{Rendered, out_path, summary};
use crate::command::{Command, CommandError, CommandKind, Context};
use crate::suggest::did_you_mean;

/// Render the assembled board in 3D: board with thickness, holes and cutouts, mask, copper
/// finish and silkscreen on its faces, and component bodies generated from package dimensions.
/// Orthographic projection, isometric by default.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Board3d {
    /// Output file, .png (default out/board3d.png, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Side facing the viewer (default top). `bottom` turns the board over, mirrored left to
    /// right like the realistic bottom view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<RealisticSide>,
    /// Where the viewer stands around the board, in degrees clockwise from the front edge
    /// (the bottom edge of the 2D view): 0 = front, 45 = front-left (default), 90 = left,
    /// -45 = front-right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azimuth: Option<f64>,
    /// Viewer height above the board plane in degrees, 5 to 90 (default 35.26, isometric;
    /// 90 looks straight down).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevation: Option<f64>,
    /// Draw component bodies (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<bool>,
    /// Designators of placed components to draw in the highlight color (orange).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub highlight: Vec<String>,
    /// Longer image side in pixels (default 1600).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    /// Resolution in pixels per millimeter; overrides `size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub px_per_mm: Option<f64>,
}

impl Command for Board3d {
    const NAME: &'static str = "render.board3d";
    const SUMMARY: &'static str =
        "Render the assembled board in 3D (isometric or any angle): board, holes, mask, silk, component bodies";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = Rendered;

    fn run(self, ctx: &mut Context<'_>) -> Result<Rendered, CommandError> {
        let p = ctx.project()?;
        let board = p.board();
        for h in &self.highlight {
            if !board.footprints.contains_key(h) {
                return Err(CommandError::not_found(
                    "render.unknown_highlight",
                    format!("`{h}` is not a placed component"),
                )
                .with_suggestions(&did_you_mean(h, board.footprints.keys().map(String::as_str), 3))
                .with_hint("highlight takes designators of placed components (U1)"));
            }
        }
        let elevation = self.elevation.unwrap_or(35.264_389_682_754_654);
        if !(5.0..=90.0).contains(&elevation) {
            return Err(CommandError::invalid_args(
                "render.bad_angle",
                format!("elevation {elevation}° is outside 5..90°"),
            )
            .with_hint("use 90 for a view straight down, 35.26 for isometric; view: bottom shows the other side"));
        }
        let azimuth = self.azimuth.unwrap_or(45.0);
        if !azimuth.is_finite() {
            return Err(CommandError::invalid_args("render.bad_angle", "azimuth must be a number of degrees")
                .with_hint("0 = from the front, 45 = front-left, 90 = left"));
        }
        let path = out_path(ctx, &self.path, "board3d.png");
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if ext != "png" {
            return Err(CommandError::invalid_args(
                "render.format",
                format!("the 3D view is written as PNG, not `.{ext}`"),
            )
            .with_hint("give a path ending in .png"));
        }
        #[cfg(feature = "png")]
        {
            use crate::model::board::BoardSide;
            use crate::render::board3d::{Options3d, render};
            let opts = Options3d {
                side: match self.view.unwrap_or(RealisticSide::Top) {
                    RealisticSide::Top => BoardSide::Top,
                    RealisticSide::Bottom => BoardSide::Bottom,
                },
                azimuth,
                elevation,
                components: self.components.unwrap_or(true),
                highlight: self.highlight.clone(),
                size: self.size.unwrap_or(1600),
                px_per_mm: self.px_per_mm,
            };
            let img = render(p, &opts).map_err(|e| {
                CommandError::invalid_args("render.empty_board", e)
                    .with_hint("define an outline (`board outline`) and place components; lower `size` for huge images")
            })?;
            let bytes = img.to_png().map_err(|e| CommandError::invalid_args("render.failed", e))?;
            let io = |e| CommandError::from(crate::model::ModelError::Io { path: path.clone(), source: e });
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(io)?;
            }
            std::fs::write(&path, bytes).map_err(io)?;
            Ok(Rendered {
                path: path.display().to_string(),
                format: "png".into(),
                width: img.width as f64,
                height: img.height as f64,
                png: Some(path.display().to_string()),
                pages: Vec::new(),
            })
        }
        #[cfg(not(feature = "png"))]
        {
            let _ = (azimuth, elevation, path);
            Err(CommandError::invalid_args("render.no_png", "this build has no PNG support")
                .with_hint("build cadlab with the `png` feature"))
        }
    }

    fn summarize(o: &Rendered) -> String {
        summary(o)
    }
}
