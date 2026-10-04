//! Zone fill as it was computed before obstacles were culled per zone (DECISIONS D42): every
//! item of the layer, every NPTH hole, every keep-out and the whole of every earlier fill go
//! into each zone's booleans. `tests/perf.rs` checks that `fill_zones_uncached` gives exactly
//! the same polygons. It follows the fill semantics otherwise (D40): per-item clearances (custom
//! rules, local clearances, net classes), per-pad zone connections and slotted NPTH holes.

use std::collections::BTreeMap;

use cadlab::board::zones::{self, FILL_TOL, OBSTACLE_TOL, SAFETY, ZoneFill, ZoneParams};
use cadlab::board::{CopperItem, ItemRef, placed_pads};
use cadlab::geom::poly::{self, Boolean, Circle, FillRule, Geometry, Join, Op, Polygon, PolygonSet, Ring};
use cadlab::model::Project;
use cadlab::model::board::PadConnection;
use cadlab::units::Nm;

fn rect(x0: i64, y0: i64, x1: i64, y1: i64) -> Polygon {
    let p = poly::Point::new;
    Polygon::new(vec![p(x0, y0), p(x1, y0), p(x1, y1), p(x0, y1)], vec![])
}

fn spokes(item: &CopperItem, gap: i64, w: i64) -> Vec<Polygon> {
    let Some(b) = item.shape.bbox() else { return vec![] };
    let (cx, cy) = (item.anchor.x.0, item.anchor.y.0);
    let h = w / 2;
    let reach = gap + w;
    vec![
        rect(cx - h, cy - h, b.max.x + reach, cy + (w - h)),
        rect(cx - h, cy - h, cx + (w - h), b.max.y + reach),
        rect(b.min.x - reach, cy - h, cx + (w - h), cy + (w - h)),
        rect(cx - h, b.min.y - reach, cx + (w - h), cy + (w - h)),
    ]
}

fn same_net(a: Option<&str>, b: Option<&str>) -> bool {
    a.is_some() && a == b
}

#[allow(clippy::too_many_arguments)]
fn fill_layer(
    net: Option<&str>,
    outline: &[poly::Point],
    board: Option<&PolygonSet>,
    items: &[(&CopperItem, Nm)],
    keepaway: Vec<Polygon>,
    prm: &ZoneParams,
) -> Result<PolygonSet, poly::Error> {
    let outline: Ring = outline.to_vec().into();
    let area = match board {
        Some(b) => Boolean::new()
            .subject(&outline, FillRule::NonZero)
            .clip(b, FillRule::NonZero)
            .op(Op::Intersection)
            .execute()?,
        None => poly::union_all(&outline, FillRule::NonZero)?,
    };
    if area.is_empty() {
        return Ok(vec![]);
    }
    let abox = area.bbox().expect("non-empty");
    let mut hard_groups: BTreeMap<i64, Vec<Polygon>> = BTreeMap::new();
    let mut soft_groups: BTreeMap<i64, Vec<Polygon>> = BTreeMap::new();
    let mut thermal: Vec<&CopperItem> = Vec::new();
    let mut same: Vec<&CopperItem> = Vec::new();
    for &(it, c) in items {
        if same_net(it.net.as_deref(), net) {
            same.push(it);
            if let ItemRef::Pad(..) = it.item {
                let d = match zones::connection(it, prm.pads) {
                    PadConnection::Solid | PadConnection::ThtThermal => continue,
                    PadConnection::Thermal => {
                        thermal.push(it);
                        prm.thermal_gap.0
                    }
                    PadConnection::None => prm.clearance.0,
                };
                soft_groups.entry(d).or_default().extend(it.shape.iter().cloned());
            }
            continue;
        }
        let c = c.max(prm.clearance).0;
        if it.shape.bbox().is_some_and(|b| b.expand(c + SAFETY + 1).intersects(&abox)) {
            hard_groups.entry(c).or_default().extend(it.shape.iter().cloned());
        }
    }
    if net.is_some() && same.is_empty() {
        return Ok(vec![]);
    }
    let mut hard: Vec<Polygon> = keepaway;
    for (c, shapes) in hard_groups {
        hard.extend(poly::offset(&shapes, c + SAFETY, Join::Round, OBSTACLE_TOL)?);
    }
    let mut soft: Vec<Polygon> = Vec::new();
    for (d, shapes) in soft_groups {
        soft.extend(poly::offset(&shapes, d, Join::Round, OBSTACLE_TOL)?);
    }
    let avail =
        Boolean::new().subject(&area, FillRule::NonZero).clip(&hard, FillRule::NonZero).op(Op::Difference).execute()?;
    let mut fill = if soft.is_empty() {
        avail.clone()
    } else {
        Boolean::new().subject(&avail, FillRule::NonZero).clip(&soft, FillRule::NonZero).op(Op::Difference).execute()?
    };
    if prm.min_width.0 > 1 && !fill.is_empty() {
        fill = poly::opening(&fill, prm.min_width.0 / 2, FILL_TOL)?;
    }
    let mut kept: Vec<Polygon> = Vec::new();
    if !thermal.is_empty() && !fill.is_empty() {
        let (gap, w) = (prm.thermal_gap.0, prm.thermal_spoke.0);
        let windows: Vec<Polygon> = thermal
            .iter()
            .filter_map(|it| it.shape.bbox())
            .map(|b| {
                let b = b.expand(gap + w + 1);
                rect(b.min.x, b.min.y, b.max.x, b.max.y)
            })
            .collect();
        let clip = |set: &PolygonSet| {
            Boolean::new()
                .subject(set, FillRule::NonZero)
                .clip(&windows, FillRule::NonZero)
                .op(Op::Intersection)
                .execute()
        };
        let (local_avail, local_fill) = (clip(&avail)?, clip(&fill)?);
        let near = |set: &PolygonSet, r: &poly::Rect| -> PolygonSet {
            set.iter().filter(|q| q.bbox().is_some_and(|b| b.intersects(r))).cloned().collect()
        };
        for it in &thermal {
            for s in spokes(it, gap, w) {
                let r = s.bbox().expect("spoke");
                if poly::contains(&near(&local_avail, &r), &s) && poly::intersects(&near(&local_fill, &r), &s) {
                    kept.push(s);
                }
            }
        }
    }
    if !kept.is_empty() {
        fill = Boolean::new().subject(&fill, FillRule::NonZero).subject(&kept, FillRule::NonZero).execute()?;
    }
    if net.is_some() {
        fill.retain(|poly| same.iter().any(|it| poly::intersects(poly, &it.shape)));
    }
    Ok(fill)
}

/// Every zone fill of the board, as `zones::fill_zones_uncached` computed them before D42.
pub fn fill_zones(p: &Project, base: &[CopperItem]) -> Vec<ZoneFill> {
    let board = p.board();
    let rules = &board.rules;
    let npth: Vec<(poly::Point, poly::Point, i64)> = placed_pads(p)
        .into_iter()
        .filter_map(|pp| match pp.hole {
            Some((d, false)) => {
                let (a, b) = pp.slot.unwrap_or((pp.center, pp.center));
                Some((a.into(), b.into(), d.0 / 2))
            }
            _ => None,
        })
        .collect();
    let area = zones::board_area(p, rules.copper_to_edge);
    let net_c = |n: Option<&str>| zones::class_clearance(p, n).unwrap_or(rules.clearance);
    // Per item: custom rule, local clearance, else the net's (D40).
    let resolved = cadlab::board::clearance::Clearances::new(p, base, true);
    let mut by_layer: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    for (zi, z) in board.zones.iter().enumerate() {
        for (li, layer) in z.layers.iter().enumerate() {
            by_layer.entry(layer.as_str()).or_default().push((zi, li));
        }
    }
    let mut all = Vec::new();
    for (layer, mut jobs) in by_layer {
        jobs.sort_by_key(|&(zi, _)| (std::cmp::Reverse(board.zones[zi].priority), zi));
        let items_on: Vec<(&CopperItem, Nm)> = base
            .iter()
            .enumerate()
            .filter(|(_, it)| it.layers.iter().any(|l| l == layer))
            .map(|(i, it)| (it, resolved.item(i)))
            .collect();
        let mut done: Vec<(usize, usize, ZoneFill)> = Vec::new();
        for (zi, li) in jobs {
            let z = &board.zones[zi];
            let mut prm = zones::zone_params(p, z);
            prm.clearance = prm.clearance.max(net_c(z.net.as_deref()));
            let result = (|| -> Result<PolygonSet, poly::Error> {
                let barea = match &area {
                    Ok(a) => a.as_ref(),
                    Err(e) => return Err(e.clone()),
                };
                let mut keepaway: Vec<Polygon> = Vec::new();
                for &(a, b, r) in &npth {
                    let r = r + prm.clearance.0 + SAFETY;
                    if a == b {
                        let ring = Circle::new(a, r).to_ring(OBSTACLE_TOL)?;
                        keepaway.push(Polygon::new(ring, vec![]));
                    } else {
                        let path = vec![poly::Path(vec![a, b])];
                        keepaway.extend(poly::offset_paths(&path, r, Join::Round, poly::EndCap::Round, OBSTACLE_TOL)?);
                    }
                }
                for (_, _, f) in &done {
                    if !same_net(f.net.as_deref(), z.net.as_deref()) && !f.fill.is_empty() {
                        let c = prm.clearance.max(f.clearance).0 + SAFETY;
                        keepaway.extend(poly::offset(&f.fill, c, Join::Round, OBSTACLE_TOL)?);
                    }
                }
                for k in &board.keepouts {
                    if k.no_pours
                        && (k.layers.is_empty() || k.layers.iter().any(|l| l == layer))
                        && k.outline.len() >= 3
                    {
                        let mut ring: Ring = k.outline.iter().map(|&q| q.into()).collect::<Vec<_>>().into();
                        if !ring.is_ccw() {
                            ring.reverse_orientation();
                        }
                        keepaway.push(Polygon::new(ring, vec![]));
                    }
                }
                let outline: Vec<poly::Point> = z.outline.iter().map(|&q| q.into()).collect();
                fill_layer(z.net.as_deref(), &outline, barea, &items_on, keepaway, &prm)
            })();
            let (fill, error) = match result {
                Ok(f) => (f, None),
                Err(e) => (vec![], Some(e.to_string())),
            };
            done.push((
                zi,
                li,
                ZoneFill {
                    zone: z.id,
                    name: z.name.clone(),
                    net: z.net.clone(),
                    layer: layer.to_string(),
                    clearance: prm.clearance,
                    fill,
                    error,
                },
            ));
        }
        all.extend(done);
    }
    all.sort_by_key(|(zi, li, _)| (*zi, *li));
    all.into_iter().map(|(_, _, f)| f).collect()
}
