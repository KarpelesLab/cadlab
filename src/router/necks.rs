//! Necks mended: the joints of new tracks that the DRC would report as `drc.neck`
//! ([`crate::drc::neck`]) made whole by a piece of track from one centre line to the other.
//!
//! A router lays tracks between points of a lattice and ends them at pads off it, and steps
//! after the search cut, shorten and take out tracks (the ends inside pads, the tracks that a
//! pour joins, D116, D127). What is left may hold together by the overlap of two round ends,
//! which connectivity counts as joined: a neck (D144). The sources are fixed where they are
//! known (D145); this step is for what is left, of either router: for each neck with a new
//! track on one side, a piece of track of the narrower width from the nearest point of the
//! track's centre line to the other's (a track's nearest point, a via's centre, a point just
//! inside a pad's copper). The piece lies between two round ends of its own net that touch, so
//! it adds copper only in the notches beside their overlap, and it is kept only if the board's
//! copper rules have nothing to say of it (`drc::check_copper_items`): a clearance that the
//! notch was needed for leaves the neck as it is, for the DRC to report.

use std::collections::BTreeSet;

use polyclip::Path;

use crate::board::{self as geo, CopperItem, ItemRef};
use crate::geom::Point;
use crate::id::ObjectId;
use crate::model::Project;
use crate::model::board::{Track, Via};
use crate::refs::ObjectRef;
use crate::units::Nm;

/// How far inside a pad's copper a mending piece ends.
const INSIDE: f64 = 10_000.0;

/// What [`mend`] made of a board's necks.
#[derive(Clone, Debug, Default)]
pub(crate) struct Mended {
    /// The pieces of track to add (identifiers not set).
    pub(crate) tracks: Vec<Track>,
    /// The necks that are left: no room for the piece, or a joint of an arc.
    pub(crate) left: usize,
}

/// A project with the new tracks and vias on it under identifiers of their own, and the
/// identifiers of the tracks, in order.
pub(crate) fn with_copper(p: &Project, tracks: &[Track], vias: &[Via]) -> (Project, Vec<ObjectId>) {
    let mut q = p.clone();
    let mut ids = Vec::with_capacity(tracks.len());
    for t in tracks {
        let mut t = t.clone();
        t.id = q.alloc_id();
        ids.push(t.id);
        q.board_mut().tracks.push(t);
    }
    for v in vias {
        let mut v = v.clone();
        v.id = q.alloc_id();
        q.board_mut().vias.push(v);
    }
    (q, ids)
}

fn fp(p: Point) -> (f64, f64) {
    (p.x.0 as f64, p.y.0 as f64)
}

fn round(p: (f64, f64)) -> Point {
    Point::new(Nm(p.0.round() as i64), Nm(p.1.round() as i64))
}

/// The point of the segment `a`-`b` nearest to `p`.
fn nearest_on(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (a.0 + t * dx, a.1 + t * dy)
}

/// The nearest points of two segments that do not cross: one of them is an end.
fn nearest_between(a: (Point, Point), b: (Point, Point)) -> (Point, Point) {
    let (a0, a1, b0, b1) = (fp(a.0), fp(a.1), fp(b.0), fp(b.1));
    let d2 = |p: (f64, f64), q: (f64, f64)| (p.0 - q.0).powi(2) + (p.1 - q.1).powi(2);
    let candidates = [
        (a0, nearest_on(a0, b0, b1)),
        (a1, nearest_on(a1, b0, b1)),
        (nearest_on(b0, a0, a1), b0),
        (nearest_on(b1, a0, a1), b1),
    ];
    let best = candidates.into_iter().min_by(|x, y| d2(x.0, x.1).total_cmp(&d2(y.0, y.1))).expect("four candidates");
    (round(best.0), round(best.1))
}

/// The pieces of track that mend the necks of `q` between a track of `new` and the rest of its
/// net, checked against the board's copper rules, and how many necks are left. `items` are the
/// copper items of `q`, with its fills (a neck under a pour is none) or without them.
pub(crate) fn mend(q: &Project, items: &[CopperItem], new: &BTreeSet<ObjectId>) -> Mended {
    let board = q.board();
    let layers = board.stackup.copper_names();
    let find = |items: &[CopperItem]| crate::drc::neck::necks(&board.rules, &layers, &board.tracks, &board.vias, items);
    let is_new = |i: usize| matches!(&items[i].item, ItemRef::Track(id) if new.contains(id));
    let track = |i: usize| match &items[i].item {
        ItemRef::Track(id) => board.tracks.iter().find(|t| t.id == *id),
        _ => None,
    };
    let necks = find(items);
    let ours: Vec<_> = necks.iter().filter(|n| is_new(n.a) || is_new(n.b)).collect();
    if ours.is_empty() {
        return Mended::default();
    }
    let mut pieces: Vec<Track> = Vec::new();
    for n in &ours {
        let Some(t) = track(n.a).filter(|t| t.mid.is_none()) else { continue };
        let ends = match &items[n.b].item {
            ItemRef::Track(_) => {
                track(n.b).filter(|u| u.mid.is_none()).map(|u| nearest_between((t.start, t.end), (u.start, u.end)))
            }
            ItemRef::Via(id) => board.vias.iter().find(|v| v.id == *id).map(|v| {
                let from = nearest_on(fp(v.at), fp(t.start), fp(t.end));
                (round(from), v.at)
            }),
            // A pad or a copper drawing: from the track's nearest point to just inside it.
            _ => {
                let line = Path(vec![t.start.into(), t.end.into()]);
                polyclip::distance(&line, &items[n.b].shape).map(|c| {
                    let (dx, dy) = (c.b.x - c.a.x, c.b.y - c.a.y);
                    let len = dx.hypot(dy);
                    let k = if len > 0.0 { INSIDE / len } else { 0.0 };
                    (round((c.a.x, c.a.y)), round((c.b.x + dx * k, c.b.y + dy * k)))
                })
            }
        };
        let Some((from, to)) = ends.filter(|(a, b)| a != b) else { continue };
        pieces.push(Track {
            id: ObjectId(0),
            layer: n.layer.clone(),
            width: Nm(n.nominal.0.min(t.width.0)),
            net: t.net.clone(),
            start: from,
            end: to,
            mid: None,
            locked: false,
        });
    }
    // The board with the pieces: those that a copper rule names are taken back.
    let mut with = q.clone();
    let mut ids: Vec<ObjectId> = Vec::with_capacity(pieces.len());
    for t in &mut pieces {
        t.id = with.alloc_id();
        ids.push(t.id);
        with.board_mut().tracks.push(t.clone());
    }
    let named: BTreeSet<u64> = crate::drc::check_copper_items(&with, &geo::base_copper_items(&with))
        .iter()
        .filter(|d| d.severity == crate::diag::Severity::Error)
        .flat_map(|d| d.subjects.iter())
        .filter_map(|s| match s {
            ObjectRef::Item { kind, index } if kind == "track" => Some(*index),
            _ => None,
        })
        .collect();
    pieces.retain(|t| !named.contains(&t.id.0));
    with.board_mut().tracks.retain(|t| !ids.contains(&t.id) || !named.contains(&t.id.0));
    // What is left, with the fills that were given: on the board's other copper as it is now.
    let mut after = geo::base_copper_items(&with);
    after.extend(items.iter().filter(|it| matches!(it.item, ItemRef::Zone(..))).cloned());
    let b = with.board();
    let kept: BTreeSet<ObjectId> = pieces.iter().map(|t| t.id).collect();
    let left = crate::drc::neck::necks(&b.rules, &layers, &b.tracks, &b.vias, &after)
        .iter()
        .filter(|n| {
            [n.a, n.b]
                .into_iter()
                .any(|i| matches!(&after[i].item, ItemRef::Track(id) if new.contains(id) || kept.contains(id)))
        })
        .count();
    for t in &mut pieces {
        t.id = ObjectId(0);
    }
    Mended { tracks: pieces, left }
}

/// Mends the necks of a router's new `tracks` on the board `p` ([`mend`]): the pieces are added
/// to `tracks`. The board is looked at without its fills first, which costs little and finds
/// every neck (and those that a pour covers); only a board with one is filled. Returns how many
/// pieces were added and how many necks are left.
pub(crate) fn mend_new(p: &Project, tracks: &mut Vec<Track>, vias: &[Via]) -> (usize, usize) {
    if tracks.is_empty() {
        return (0, 0);
    }
    let (q, ids) = with_copper(p, tracks, vias);
    let new: BTreeSet<ObjectId> = ids.into_iter().collect();
    let board = q.board();
    let base = geo::base_copper_items(&q);
    let ours = |items: &[CopperItem]| {
        crate::drc::neck::necks(&board.rules, &board.stackup.copper_names(), &board.tracks, &board.vias, items)
            .iter()
            .any(|n| [n.a, n.b].into_iter().any(|i| matches!(&items[i].item, ItemRef::Track(id) if new.contains(id))))
    };
    if !ours(&base) {
        return (0, 0);
    }
    let items = geo::copper_items(&q);
    let m = mend(&q, &items, &new);
    let added = m.tracks.len();
    tracks.extend(m.tracks);
    (added, m.left)
}

/// What the step says of itself.
pub(crate) fn said(mended: usize, left: usize) -> String {
    let rest = match left {
        0 => String::new(),
        n => format!(", {n} left as they are (no room for the piece: `drc.neck` has them)"),
    };
    format!("{mended} joint(s) of round ends alone made whole by a piece of track{rest}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_points_of_two_segments() {
        let p = |x: i64, y: i64| Point::new(Nm(x), Nm(y));
        // End to end on a line, 200 apart.
        assert_eq!(nearest_between((p(0, 0), p(1000, 0)), (p(1200, 0), p(2000, 0))), (p(1000, 0), p(1200, 0)));
        // An end beside the middle of the other.
        assert_eq!(nearest_between((p(500, 100), p(500, 900)), (p(0, 0), p(1000, 0))), (p(500, 100), p(500, 0)));
        assert_eq!(nearest_between((p(0, 0), p(1000, 0)), (p(500, 100), p(500, 900))), (p(500, 0), p(500, 100)));
    }
}
