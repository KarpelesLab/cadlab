//! Slanted tracks against the mesh: a track oblique to the mesh lines gets no lines of its own, and
//! one narrower than the cells under it can come out in pieces. [`narrow`] finds them, for the mesh
//! refinement of [`super::extract`] and the warning of an explicit `max_cell`.

use crate::geom::poly::{Point, Polygon};

/// The slanted tracks of a shape too narrow for the mesh: polygons whose width (twice area over
/// perimeter) is below `cells` of the larger cell under their longest slanted edge, that edge being
/// at least three widths long (a track, not a round pad). Their width and that cell, in nm.
pub(crate) fn narrow(shape: &[Polygon], xs: &[i64], ys: &[i64], cells: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for q in shape {
        let (mut area, mut perimeter) = (0.0, 0.0);
        let mut longest: Option<(f64, Point, Point)> = None;
        for (ri, r) in q.rings().enumerate() {
            let mut twice = 0.0;
            for (a, b) in r.edges() {
                let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
                let len = dx.hypot(dy);
                perimeter += len;
                twice += a.x as f64 * b.y as f64 - b.x as f64 * a.y as f64;
                if a.x != b.x && a.y != b.y && longest.is_none_or(|l| len > l.0) {
                    longest = Some((len, a, b));
                }
            }
            area += if ri == 0 { twice.abs() / 2.0 } else { -twice.abs() / 2.0 };
        }
        let Some((length, a, b)) = longest else { continue };
        let cell_at = |l: &[i64], v: i64| {
            let i = l.partition_point(|&x| x <= v).clamp(1, l.len() - 1);
            (l[i] - l[i - 1]) as f64
        };
        let cell = cell_at(xs, a.x + (b.x - a.x) / 2).max(cell_at(ys, a.y + (b.y - a.y) / 2));
        let width = 2.0 * area / perimeter;
        if width < cells * cell && length >= 3.0 * width {
            out.push((width, cell));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: i64, d: i64) -> Vec<i64> {
        (0..=n).map(|i| i * d).collect()
    }

    /// A 48-sided polygon around a disc.
    fn disc(x: i64, y: i64, r: i64) -> Polygon {
        let t = std::f64::consts::TAU / 48.0;
        let ring: Vec<Point> = (0..48)
            .map(|k| {
                let a = t * k as f64;
                Point::new(x + (r as f64 * a.cos()).round() as i64, y + (r as f64 * a.sin()).round() as i64)
            })
            .collect();
        Polygon::new(ring, Vec::new())
    }

    #[test]
    fn narrow_finds_tracks_not_round_pads() {
        let (xs, ys) = (lines(40, 1000), lines(40, 1000));
        let track: Vec<Point> =
            [(1000, 0), (20_000, 19_000), (19_000, 20_000), (0, 1000)].iter().map(|&(x, y)| Point::new(x, y)).collect();
        assert_eq!(narrow(&[Polygon::new(track, Vec::new())], &xs, &ys, 1.5).len(), 1);
        assert!(narrow(&[disc(20_000, 20_000, 500)], &xs, &ys, 1.5).is_empty());
    }
}
