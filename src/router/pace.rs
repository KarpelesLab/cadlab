//! The machine's pace: how fast this machine searches a grid, beside the machine that the
//! counts made from a time budget were fitted on (D147).
//!
//! A time budget cannot give the same board on every machine. What it can give is the same
//! board on every run of one machine: a stage that the clock ended is ended by a count of its
//! own operations instead, and the count is made from the budget's seconds and the rate at
//! which the machine does those operations. That rate is not a constant of the build (D146
//! had 2.8 million nodes a second for each thread, fitted on the machine of the benchmarks):
//! it is measured here, once in a process, by a small search that is the same on every run,
//! and told as a step of the square root of two from the reference, so that the noise of a
//! timing gives the same step and with it the same count.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::OnceLock;
use std::time::Instant;

/// The side of the grid that the measuring search runs on: its costs and distances are some
/// 6 MB, past the caches of a core as a board's grids are.
const SIDE: usize = 1024;

/// The nodes that one measuring search settles.
const NODES: u32 = 120_000;

/// How many times the search is timed; the fastest counts, since what disturbs a timing only
/// ever makes it longer.
const TIMES: usize = 7;

/// The nodes a second of the measuring search on the machine of the benchmarks, four runs
/// at a time on three threads each (D147): the pace that is 1.
const REFERENCE: f64 = 26.5e6;

/// What a run is told of the machine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pace {
    /// The nodes a second of the measuring search, as timed.
    pub measured: f64,
    /// The steps of the square root of two that this is from the reference, to the nearest.
    pub step: i32,
}

impl Pace {
    /// The machine's rate as a multiple of the reference's: a power of the square root of two.
    pub(crate) fn factor(self) -> f64 {
        2f64.powf(f64::from(self.step) / 2.0)
    }

    fn of(measured: f64) -> Pace {
        let step = if measured > 0.0 { (2.0 * (measured / REFERENCE).log2()).round() } else { 0.0 };
        Pace { measured, step: step.clamp(-12.0, 12.0) as i32 }
    }
}

/// The pace of this machine, measured at the first call in a process (some 0.1 s) and the same
/// for every run after it. `CADLAB_PACE_STEP` gives the step in place of the measurement, for
/// measuring what another machine would be told.
pub(crate) fn pace() -> Pace {
    static PACE: OnceLock<Pace> = OnceLock::new();
    *PACE.get_or_init(|| {
        let measured = measure();
        match std::env::var("CADLAB_PACE_STEP").ok().and_then(|v| v.parse::<i32>().ok()) {
            Some(step) => Pace { measured, step: step.clamp(-12, 12) },
            None => Pace::of(measured),
        }
    })
}

/// The nodes a second of the measuring search: the fastest of [`TIMES`].
fn measure() -> f64 {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let cost: Vec<u16> = (0..SIDE * SIDE)
        .map(|_| {
            // splitmix64: the same costs on every machine.
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            1 + ((z ^ (z >> 31)) % 15) as u16
        })
        .collect();
    let mut dist = vec![u32::MAX; SIDE * SIDE];
    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    let mut best = f64::MAX;
    let mut sum = 0u64;
    for _ in 0..TIMES {
        dist.fill(u32::MAX);
        heap.clear();
        let from = (SIDE / 2 * SIDE + SIDE / 2) as u32;
        dist[from as usize] = 0;
        heap.push(Reverse((0, from)));
        let began = Instant::now();
        let mut settled = 0u32;
        while let Some(Reverse((d, at))) = heap.pop() {
            if d > dist[at as usize] {
                continue;
            }
            settled += 1;
            if settled >= NODES {
                break;
            }
            let (x, y) = (at as usize % SIDE, at as usize / SIDE);
            let mut step = |to: usize| {
                let nd = d + u32::from(cost[to]);
                if nd < dist[to] {
                    dist[to] = nd;
                    heap.push(Reverse((nd, to as u32)));
                }
            };
            if x > 0 {
                step(at as usize - 1);
            }
            if x + 1 < SIDE {
                step(at as usize + 1);
            }
            if y > 0 {
                step(at as usize - SIDE);
            }
            if y + 1 < SIDE {
                step(at as usize + SIDE);
            }
        }
        let took = began.elapsed().as_secs_f64();
        sum = sum.wrapping_add(u64::from(settled));
        if took > 0.0 {
            best = best.min(took);
        }
    }
    std::hint::black_box(sum);
    if best == f64::MAX { 0.0 } else { f64::from(NODES) / best }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_is_told_as_the_nearest_step_of_the_square_root_of_two() {
        assert_eq!(Pace::of(REFERENCE).step, 0);
        // Within a sixth either way of the reference: the same step, and the same count.
        assert_eq!(Pace::of(REFERENCE * 1.15).step, 0);
        assert_eq!(Pace::of(REFERENCE * 0.87).step, 0);
        // Half as fast: two steps down, and half the count.
        let half = Pace::of(REFERENCE * 0.5);
        assert_eq!(half.step, -2);
        assert!((half.factor() - 0.5).abs() < 1e-9);
        assert_eq!(Pace::of(REFERENCE * 1.4).step, 1);
        assert_eq!(Pace::of(0.0).step, 0);
    }

    #[test]
    fn the_machine_has_a_pace() {
        let p = pace();
        assert!(p.measured > 0.0 && (-12..=12).contains(&p.step), "{p:?}");
        assert_eq!(pace(), p, "measured once in a process");
    }
}
