//! Reproduction of the zone fill cost left in `polyclip` (docs/POLYGON_LIB.md, "Wishlist from
//! cadlab"): the GND pour of the synthetic large board (`tests/common/bigboard.rs`) before its
//! minimum-width opening, and the `polyclip` calls the fill makes on it, timed one by one.
//!
//! `cargo run --release --example polyclip_opening [out.json]` prints the timings; with a path,
//! it also writes the input polygon set (polyclip's serde format), so the case can be replayed
//! with polyclip alone:
//!
//! ```text
//! let set: polyclip::PolygonSet = serde_json::from_str(&std::fs::read_to_string(path)?)?;
//! polyclip::opening(&set, 100_000, polyclip::ArcTol::new(5_000, polyclip::Side::Inside))?;
//! ```

#[path = "../tests/common/bigboard.rs"]
mod bigboard;

use std::time::{Duration, Instant};

use cadlab::board::zones::{self, FILL_TOL, LayerInput};
use cadlab::geom::poly::{self, Boolean, FillRule, Join, Polygon, PolygonSet};
use cadlab::model::board::PadConnection;

fn best<T>(f: impl Fn() -> T) -> (Duration, T) {
    let mut d = Duration::MAX;
    let mut out = None;
    for _ in 0..3 {
        let t = Instant::now();
        out = Some(f());
        d = d.min(t.elapsed());
    }
    (d, out.unwrap())
}

fn vertices(s: &PolygonSet) -> usize {
    s.iter().map(|p| p.vertex_count()).sum()
}

fn main() {
    let (_dir, _r, s) = bigboard::build(bigboard::Spec::default());
    let p = s.project.as_ref().unwrap();
    let base = cadlab::board::base_copper_items(p);
    let z = p.board().zones.iter().find(|z| z.name == "GND_In1").expect("GND pour");
    let layer = "In1.Cu";
    let mut prm = zones::zone_params(p, z);
    let width = prm.min_width.0 / 2;
    // The fill as it enters the opening: no opening, no spokes.
    prm.min_width = cadlab::Nm(0);
    prm.pads = PadConnection::None;
    prm.clearance = prm.thermal_gap.max(prm.clearance);
    let outline: Vec<poly::Point> = z.outline.iter().map(|&q| q.into()).collect();
    let area = zones::board_area(p, p.board().rules.copper_to_edge).unwrap();
    let items = base
        .iter()
        .filter(|it| it.layers.iter().any(|l| l == layer))
        .map(|it| (it, zones::class_clearance(p, it.net.as_deref()).unwrap_or(p.board().rules.clearance)))
        .collect();
    let input = LayerInput {
        net: z.net.as_deref(),
        outline: &outline,
        board: area.as_ref(),
        items,
        keepaway: vec![],
        params: prm,
    };
    let fill = zones::fill_layer(&input).unwrap();
    println!(
        "input: {} polygons, {} holes, {} vertices; opening by {} nm",
        fill.len(),
        fill.iter().map(|q| q.holes.len()).sum::<usize>(),
        vertices(&fill),
        width
    );
    if let Some(path) = std::env::args().nth(1) {
        std::fs::write(&path, serde_json::to_string(&fill).unwrap()).unwrap();
        println!("written to {path}");
    }
    let (d, shrunk) = best(|| poly::offset(&fill, -width, Join::Round, FILL_TOL).unwrap());
    println!("offset(-{width}): {:>7.1} ms -> {} vertices", ms(d), vertices(&shrunk));
    let (d, norm) = best(|| poly::union_all(&fill, FillRule::NonZero).unwrap());
    println!(
        "  its first step, normalizing the canonical input: {:>7.1} ms (output identical: {})",
        ms(d),
        norm == fill
    );
    let (d, grown) = best(|| poly::offset(&shrunk, width, Join::Round, FILL_TOL).unwrap());
    println!("offset(+{width}): {:>7.1} ms -> {} vertices", ms(d), vertices(&grown));
    let (d, norm) = best(|| poly::union_all(&shrunk, FillRule::NonZero).unwrap());
    println!(
        "  its first step, normalizing the canonical input: {:>7.1} ms (output identical: {})",
        ms(d),
        norm == shrunk
    );
    let (d, opened) = best(|| poly::opening(&fill, width, FILL_TOL).unwrap());
    println!("opening: {:>7.1} ms (same as the two offsets: {})", ms(d), opened == grown);
    // Spokes: a few small rectangles merged into the large set.
    let r = |x: i64, y: i64| {
        let q = poly::Point::new;
        Polygon::new(vec![q(x, y), q(x + 250_000, y), q(x + 250_000, y + 1_000_000), q(x, y + 1_000_000)], vec![])
    };
    let spokes: Vec<Polygon> = (0..40).map(|k| r(10_000_000 + k * 3_000_000, 50_000_000)).collect();
    let (d, _) = best(|| {
        Boolean::new().subject(&opened, FillRule::NonZero).subject(&spokes, FillRule::NonZero).execute().unwrap()
    });
    println!("union with 40 small rectangles: {:>7.1} ms", ms(d));
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}
