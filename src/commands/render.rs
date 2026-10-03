//! `render.*` (images) and `schematic.*` (layout hints).

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::util;
use crate::command::{Command, CommandError, CommandKind, Context, Registry};
use crate::geom::Point;
use crate::model::footprint::{GraphicGeometry, GraphicLayer, PadKind, PadShape};
use crate::render::{Color, HAlign, Scene, VAlign, View};
use crate::schematic::{self, Placement};
use crate::units::{LengthUnit, Nm};

pub(crate) fn register(r: &mut Registry) {
    r.register::<Schematic>().register::<Symbol>().register::<Footprint>().register::<Place>().register::<Unplace>();
}

/// Default resolution: 10 px/mm (about 254 dpi).
fn default_res() -> f64 {
    10.0
}

/// Result of a render.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Rendered {
    /// File written.
    pub path: String,
    /// `png` or `svg`.
    pub format: String,
    /// Size in pixels (PNG) or millimeters (SVG).
    pub width: f64,
    /// Height.
    pub height: f64,
    /// For PNG output, the image path; MCP clients receive the image itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub png: Option<String>,
}

fn out_path(ctx: &Context<'_>, path: &Option<PathBuf>, default: &str) -> PathBuf {
    let p = path.clone().unwrap_or_else(|| PathBuf::from("out").join(default));
    match ctx.session.root() {
        Some(root) if p.is_relative() => root.join(p),
        _ => p,
    }
}

fn write_scene(scene: &Scene, view: &View, path: &Path) -> Result<Rendered, CommandError> {
    let io = |e| CommandError::from(crate::model::ModelError::Io { path: path.to_path_buf(), source: e });
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase();
    let (x0, y0, x1, y1) = view.area;
    match ext.as_str() {
        "svg" => {
            std::fs::write(path, crate::render::to_svg(scene, view)).map_err(io)?;
            Ok(Rendered {
                path: path.display().to_string(),
                format: "svg".into(),
                width: x1 - x0,
                height: y1 - y0,
                png: None,
            })
        }
        "png" => {
            #[cfg(feature = "png")]
            {
                let bytes =
                    crate::render::to_png(scene, view).map_err(|e| CommandError::invalid_args("render.failed", e))?;
                std::fs::write(path, bytes).map_err(io)?;
                let (w, h) = view.pixels();
                Ok(Rendered {
                    path: path.display().to_string(),
                    format: "png".into(),
                    width: w as f64,
                    height: h as f64,
                    png: Some(path.display().to_string()),
                })
            }
            #[cfg(not(feature = "png"))]
            Err(CommandError::invalid_args("render.no_png", "this build has no PNG support; use .svg"))
        }
        other => Err(CommandError::invalid_args(
            "render.format",
            format!("unsupported image format `.{other}`; use .png or .svg"),
        )),
    }
}

fn summary(o: &Rendered) -> String {
    if o.format == "png" {
        format!("wrote {} ({} x {} px)", o.path, o.width, o.height)
    } else {
        format!("wrote {} ({:.1} x {:.1} mm)", o.path, o.width, o.height)
    }
}

/// Render the schematic (generated layout) to PNG or SVG.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Schematic {
    /// Output file, .png or .svg (default out/schematic.png, relative to the project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// PNG resolution in pixels per millimeter (default 10, about 254 dpi).
    #[serde(default = "default_res")]
    pub px_per_mm: f64,
}

impl Command for Schematic {
    const NAME: &'static str = "render.schematic";
    const SUMMARY: &'static str = "Render the schematic (automatic layout) to PNG or SVG";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["path"];
    type Output = Rendered;

    fn run(self, ctx: &mut Context<'_>) -> Result<Rendered, CommandError> {
        let p = ctx.project()?;
        let hints = p.schematic().map(|s| s.placements.clone()).unwrap_or_default();
        let layout = schematic::layout(p, &hints);
        let scene = schematic::draw(p, &layout);
        let (w, h) = (layout.size.0.to_f64(LengthUnit::Mm), layout.size.1.to_f64(LengthUnit::Mm));
        let view = View { area: (0.0, 0.0, w, h), px_per_mm: self.px_per_mm.clamp(1.0, 40.0) };
        let path = out_path(ctx, &self.path, "schematic.png");
        write_scene(&scene, &view, &path)
    }

    fn summarize(o: &Rendered) -> String {
        summary(o)
    }
}

/// Render a part's schematic symbol.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Symbol {
    /// Part ID or MPN.
    pub part: String,
    /// Output file, .png or .svg (default out/symbol-<part>.png).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// PNG resolution in pixels per millimeter.
    #[serde(default = "default_res")]
    pub px_per_mm: f64,
}

impl Command for Symbol {
    const NAME: &'static str = "render.symbol";
    const SUMMARY: &'static str = "Render a part's schematic symbol to PNG or SVG";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["part"];
    type Output = Rendered;

    fn run(self, ctx: &mut Context<'_>) -> Result<Rendered, CommandError> {
        let part = util::part_in(ctx, &self.part)?.clone();
        let sym = schematic::symbol::symbol_of(&part);
        let mut scene = Scene::default();
        let pl = Placement { at: Point::ORIGIN, rot: 0 };
        schematic::symbol::draw(&mut scene, &sym, &pl, part.category.refdes_prefix(), &part.value());
        let view = View::fit(&scene, 2.0, self.px_per_mm.clamp(1.0, 80.0));
        let path = out_path(ctx, &self.path, &format!("symbol-{}.png", part.id));
        write_scene(&scene, &view, &path)
    }

    fn summarize(o: &Rendered) -> String {
        summary(o)
    }
}

/// Render a footprint: copper pads, courtyard, silkscreen, fabrication outline.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Footprint {
    /// Footprint name, or a part ID (its preferred footprint).
    pub name: String,
    /// Output file, .png or .svg (default out/footprint-<name>.png).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// PNG resolution in pixels per millimeter.
    #[serde(default = "default_px_footprint")]
    pub px_per_mm: f64,
}

fn default_px_footprint() -> f64 {
    80.0
}

fn mm(v: Nm) -> f64 {
    v.to_f64(LengthUnit::Mm)
}

/// Draws a footprint (dark background, board-viewer colors).
pub(crate) fn footprint_scene(f: &crate::model::footprint::Footprint) -> Scene {
    let mut s = Scene { background: Color::hex(0x101418), prims: Vec::new() };
    let copper = Color::hex(0xc83434);
    let paste = Color::hex(0x8a8a8a).alpha(170);
    let drill = Color::hex(0x101418);
    for g in &f.graphics {
        let color = match g.layer {
            GraphicLayer::Silk => Color::hex(0xf2f2f2),
            GraphicLayer::Fab => Color::hex(0xb0a060),
            GraphicLayer::Courtyard => Color::hex(0xd040d0),
        };
        let w = mm(g.width).max(0.03);
        match &g.geometry {
            GraphicGeometry::Path { points } => s.line(points.iter().map(|p| (mm(p.x), mm(p.y))).collect(), w, color),
            GraphicGeometry::Polygon { points } => {
                s.outline(points.iter().map(|p| (mm(p.x), mm(p.y))).collect(), w, color)
            }
            GraphicGeometry::Circle { center, radius, filled } => {
                let c = (mm(center.x), mm(center.y));
                if *filled {
                    s.circle(c, mm(*radius), Some(color), None)
                } else {
                    s.circle(c, mm(*radius), None, Some((w, color)))
                }
            }
        }
    }
    s.outline(f.courtyard.iter().map(|p| (mm(p.x), mm(p.y))).collect(), 0.05, Color::hex(0xd040d0));
    for p in &f.pads {
        let (cx, cy) = (mm(p.at.x), mm(p.at.y));
        let rect = |w: f64, h: f64| {
            vec![
                (cx - w / 2.0, cy - h / 2.0),
                (cx + w / 2.0, cy - h / 2.0),
                (cx + w / 2.0, cy + h / 2.0),
                (cx - w / 2.0, cy + h / 2.0),
            ]
        };
        match p.shape {
            PadShape::Rect { w, h } => s.fill(rect(mm(w), mm(h)), copper, None),
            PadShape::RoundRect { w, h, r } => s.fill(round_rect(cx, cy, mm(w), mm(h), mm(r)), copper, None),
            PadShape::Oval { w, h } => {
                let r = mm(w.min(h)) / 2.0;
                s.fill(round_rect(cx, cy, mm(w), mm(h), r), copper, None);
            }
            PadShape::Circle { d } => s.circle((cx, cy), mm(d) / 2.0, Some(copper), None),
        }
        if let Some(crate::model::footprint::Paste::Windows { size, at }) = &p.paste {
            for w in at {
                let (wx, wy) = (cx + mm(w.x), cy + mm(w.y));
                let (sw, sh) = (mm(size.0), mm(size.1));
                s.fill(
                    vec![
                        (wx - sw / 2.0, wy - sh / 2.0),
                        (wx + sw / 2.0, wy - sh / 2.0),
                        (wx + sw / 2.0, wy + sh / 2.0),
                        (wx - sw / 2.0, wy + sh / 2.0),
                    ],
                    paste,
                    None,
                );
            }
        }
        if let PadKind::Tht { drill: d } | PadKind::Npth { drill: d } = p.kind {
            s.circle((cx, cy), mm(d) / 2.0, Some(drill), None);
        }
        let (pw, ph) = p.shape.size();
        let size = (mm(pw.min(ph)) * 0.5).clamp(0.2, 1.0);
        s.text(&p.number, (cx, cy), size, HAlign::Center, VAlign::Middle, 0, Color::hex(0xffffff));
    }
    s
}

fn round_rect(cx: f64, cy: f64, w: f64, h: f64, r: f64) -> Vec<(f64, f64)> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let (hw, hh) = (w / 2.0 - r, h / 2.0 - r);
    let mut pts = Vec::new();
    for (k, (ox, oy)) in [(hw, hh), (-hw, hh), (-hw, -hh), (hw, -hh)].iter().enumerate() {
        for i in 0..=6 {
            let a = std::f64::consts::FRAC_PI_2 * (k as f64 + i as f64 / 6.0);
            pts.push((cx + ox + r * a.cos(), cy + oy + r * a.sin()));
        }
    }
    pts
}

impl Command for Footprint {
    const NAME: &'static str = "render.footprint";
    const SUMMARY: &'static str = "Render a footprint: pads, paste, courtyard, silkscreen, fab outline";
    const KIND: CommandKind = CommandKind::Query;
    const POSITIONAL: &'static [&'static str] = &["name"];
    type Output = Rendered;

    fn run(self, ctx: &mut Context<'_>) -> Result<Rendered, CommandError> {
        let p = ctx.project()?;
        let fp = match util::footprint(p, &self.name) {
            Ok(f) => f.clone(),
            Err(e) => match util::part(p, &self.name)
                .ok()
                .and_then(|pt| pt.footprint())
                .and_then(|r| p.library().footprints.get(&r.footprint))
            {
                Some(f) => f.clone(),
                None => return Err(e),
            },
        };
        let scene = footprint_scene(&fp);
        let view = View::fit(&scene, 0.5, self.px_per_mm.clamp(5.0, 400.0));
        let path = out_path(ctx, &self.path, &format!("footprint-{}.png", fp.name));
        write_scene(&scene, &view, &path)
    }

    fn summarize(o: &Rendered) -> String {
        summary(o)
    }
}

/// Pin a component's symbol to a position on the schematic sheet.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Place {
    /// Designator.
    pub refdes: String,
    /// Symbol origin, Y up from the bottom-left of the sheet: ["100mm", "80mm"]. Snapped to 1.27 mm.
    pub at: Point,
    /// Counter-clockwise quarter turns (0..3).
    #[serde(default)]
    pub rot: u8,
}

/// Fixed placements after the change.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Placements {
    /// Fixed placements by designator.
    pub placements: std::collections::BTreeMap<String, Placement>,
}

impl Command for Place {
    const NAME: &'static str = "schematic.place";
    const SUMMARY: &'static str = "Fix a symbol's position on the schematic (overrides the automatic layout)";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Placements;

    fn run(self, ctx: &mut Context<'_>) -> Result<Placements, CommandError> {
        let r = util::refdes_key(ctx.project()?, &self.refdes)?;
        let half = 1_270_000;
        let snap = |v: Nm| Nm((v.0 as f64 / half as f64).round() as i64 * half);
        let pl = Placement { at: Point::new(snap(self.at.x), snap(self.at.y)), rot: self.rot % 4 };
        let s = ctx.project_mut()?.schematic_mut();
        s.placements.insert(r, pl);
        Ok(Placements { placements: s.placements.clone() })
    }

    fn summarize(o: &Placements) -> String {
        format!("{} fixed placement(s)", o.placements.len())
    }
}

/// Return components to automatic placement.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Unplace {
    /// Designators; empty means all.
    #[serde(default)]
    pub refdes: Vec<String>,
}

impl Command for Unplace {
    const NAME: &'static str = "schematic.unplace";
    const SUMMARY: &'static str = "Return components to automatic schematic placement";
    const KIND: CommandKind = CommandKind::Mutation;
    const POSITIONAL: &'static [&'static str] = &["refdes"];
    type Output = Placements;

    fn run(self, ctx: &mut Context<'_>) -> Result<Placements, CommandError> {
        let p = ctx.project_mut()?;
        if self.refdes.is_empty() {
            p.clear_schematic();
            return Ok(Placements { placements: Default::default() });
        }
        let s = p.schematic_mut();
        for r in &self.refdes {
            s.placements.remove(r);
        }
        let out = s.placements.clone();
        if out.is_empty() {
            p.clear_schematic();
        }
        Ok(Placements { placements: out })
    }

    fn summarize(o: &Placements) -> String {
        format!("{} fixed placement(s)", o.placements.len())
    }
}
