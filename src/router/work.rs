//! Work accounting: a deterministic measure of what a routing run did, and a budget in it.
//!
//! A wall-clock budget makes a run that exhausts it depend on the machine and its load: where
//! the budget ends, and so which negotiation iteration is the last, differs from run to run. A
//! *work budget* ends where a count of the run's own operations reaches a limit, so two runs of
//! the same build on the same input give the same result on any machine, under any load and with
//! any number of threads.
//!
//! One unit is about one expansion of the grid search; dearer operations (gridless searches,
//! exact checks, the verification DRC) are charged by the size of what they process, with weights
//! measured on the corpus so that a unit takes about the same time in every phase (docs/ROUTER.md,
//! "Work budget").
//!
//! Determinism with threads: the searches of a parallel batch charge a pending count, which only
//! joins the count that stop tests read when the batch ends. Each search's charge depends on the
//! data alone, so the sum after a batch does not depend on the threads, and within a batch every
//! search sees the same count.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Work of one expansion of the coupled pair search (exact checks, cached per cell).
pub(crate) const PAIR_EXPANSION: u64 = 20;

/// Work of one node of the gridless search (edges to every point of a room).
pub(crate) const GRIDLESS_EXPANSION: u64 = 2;

/// Work of building one room of a gridless search region (polygon clipping and decomposition).
pub(crate) const GRIDLESS_ROOM: u64 = 25;

/// Double grid points per layer and rule profile per unit of setup work.
pub(crate) const SETUP_POINTS: u64 = 1;

/// Halo keys claimed per unit of work.
pub(crate) const COMMIT_KEYS: u64 = 8;

/// Work of optimizing one vertex of the final paths (exact checks).
pub(crate) const POST_VERTEX: u64 = 800;

/// Work of filling the pours again around the routing, per track and via.
pub(crate) const POUR_ITEM: u64 = 150;

/// Work of the verification DRC per track and via.
pub(crate) const VERIFY_ITEM: u64 = 1000;

#[derive(Debug, Default)]
struct Shared {
    limit: Option<u64>,
    done: AtomicU64,
    pending: AtomicU64,
    in_batch: AtomicBool,
}

/// Counts the work of a routing run, and optionally limits it. Clones share one count, so nested
/// runs (re-plans, push-and-shove of leftovers) draw on one budget.
#[derive(Clone, Debug, Default)]
pub struct WorkMeter(Arc<Shared>);

impl WorkMeter {
    /// A meter that stops the run at `units` of work.
    pub fn limited(units: u64) -> WorkMeter {
        WorkMeter(Arc::new(Shared { limit: Some(units), ..Default::default() }))
    }

    /// The limit, if any.
    pub fn limit(&self) -> Option<u64> {
        self.0.limit
    }

    /// Work done so far (without the parallel batch in progress).
    pub fn used(&self) -> u64 {
        self.0.done.load(Ordering::Relaxed)
    }

    /// Work left of the limit.
    pub(crate) fn left(&self) -> Option<u64> {
        self.0.limit.map(|l| l.saturating_sub(self.used()))
    }

    /// Adds `units` of work.
    #[inline]
    pub(crate) fn charge(&self, units: u64) {
        let to = if self.0.in_batch.load(Ordering::Relaxed) { &self.0.pending } else { &self.0.done };
        to.fetch_add(units, Ordering::Relaxed);
    }

    /// From here on charges are held back until [`WorkMeter::end_batch`] (see the module docs).
    pub(crate) fn begin_batch(&self) {
        self.0.in_batch.store(true, Ordering::Relaxed);
    }

    /// Everything charged since [`WorkMeter::begin_batch`] counts now.
    pub(crate) fn end_batch(&self) {
        self.0.in_batch.store(false, Ordering::Relaxed);
        let n = self.0.pending.swap(0, Ordering::Relaxed);
        self.0.done.fetch_add(n, Ordering::Relaxed);
    }
}

/// Charges a search's node expansions: `tick` per expansion, the rest when dropped, so every way
/// out of a search is counted. Flushes in steps, so that a stop test polled right after a tick at
/// a multiple of the step sees the search's own work in a sequential phase.
pub(crate) struct Tally<'a> {
    meter: &'a WorkMeter,
    weight: u64,
    open: u64,
    step: u64,
}

impl<'a> Tally<'a> {
    /// `step` must be the period of the search's stop test.
    pub(crate) fn new(meter: &'a WorkMeter, weight: u64, step: u64) -> Tally<'a> {
        Tally { meter, weight, open: 0, step }
    }

    #[inline]
    pub(crate) fn tick(&mut self) {
        self.open += 1;
        if self.open == self.step {
            self.meter.charge(self.open * self.weight);
            self.open = 0;
        }
    }
}

impl Drop for Tally<'_> {
    fn drop(&mut self) {
        self.meter.charge(self.open * self.weight);
    }
}

/// A point at which a phase of a run ends: a time, an amount of work, both or neither.
#[derive(Clone, Debug)]
pub(crate) struct Limit {
    wall: Option<Instant>,
    work: Option<u64>,
    meter: WorkMeter,
}

impl Limit {
    /// The end of a phase taking the share `f` of a budget: `wall` from `start`, and what the
    /// meter had left at `start_work`.
    pub(crate) fn share(wall: Option<Duration>, start: Instant, meter: &WorkMeter, start_work: u64, f: f64) -> Limit {
        Limit {
            wall: wall.map(|b| start + b.mul_f64(f)),
            work: meter.limit().map(|l| start_work + (l.saturating_sub(start_work) as f64 * f) as u64),
            meter: meter.clone(),
        }
    }

    /// The meter this limit reads.
    pub(crate) fn meter(&self) -> &WorkMeter {
        &self.meter
    }

    /// Whether the phase is over.
    pub(crate) fn passed(&self) -> bool {
        self.work.is_some_and(|w| self.meter.used() > w) || self.wall.is_some_and(|d| Instant::now() > d)
    }

    /// The same limit brought forward by `span`.
    pub(crate) fn before(&self, span: Span) -> Limit {
        Limit {
            wall: self.wall.map(|d| d.checked_sub(span.wall).unwrap_or(d)),
            work: self.work.map(|w| w.saturating_sub(span.work)),
            meter: self.meter.clone(),
        }
    }
}

/// An amount of a run's budget, in time and in work.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Span {
    pub(crate) wall: Duration,
    pub(crate) work: u64,
}

impl Span {
    /// What has passed since (`start`, `start_work`).
    pub(crate) fn since(start: Instant, meter: &WorkMeter, start_work: u64) -> Span {
        Span { wall: start.elapsed(), work: meter.used().saturating_sub(start_work) }
    }

    /// This span times `f`.
    pub(crate) fn scaled(self, f: f64) -> Span {
        Span { wall: self.wall.mul_f64(f), work: (self.work as f64 * f) as u64 }
    }

    /// `n` times this span.
    pub(crate) fn times(self, n: u32) -> Span {
        Span { wall: self.wall * n, work: self.work * u64::from(n) }
    }

    /// Whether this span is at most the share `f` of a budget (time, and the meter's limit).
    pub(crate) fn at_most_share(self, f: f64, wall: Option<Duration>, meter: &WorkMeter, start_work: u64) -> bool {
        wall.is_none_or(|b| self.wall <= b.mul_f64(f))
            && meter.limit().is_none_or(|l| self.work as f64 <= l.saturating_sub(start_work) as f64 * f)
    }
}

/// Work of optimizing, verifying and reporting per connection, estimated before routing: about
/// six tracks and seven vertices per connection on the corpus ([`VERIFY_ITEM`], [`POST_VERTEX`]).
const TAIL_PER_CONNECTION: u64 = 6 * VERIFY_ITEM + 7 * POST_VERTEX;

/// Fewest nanoseconds per unit of work assumed when a run's setup is too short to tell.
const MIN_UNIT_NS: f64 = 200.0;

/// What the end of a run of `connections` will take, in work and in time at the speed of its
/// `setup` (never assumed faster than [`MIN_UNIT_NS`]).
pub(crate) fn tail_estimate(connections: usize, setup: Span) -> Span {
    let work = connections as u64 * TAIL_PER_CONNECTION;
    let unit = if setup.work > 0 { setup.wall.as_nanos() as f64 / setup.work as f64 } else { 0.0 };
    Span { wall: Duration::from_nanos((work as f64 * unit.max(MIN_UNIT_NS)) as u64), work }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_charges_count_only_when_the_batch_ends() {
        let m = WorkMeter::limited(100);
        m.charge(10);
        assert_eq!(m.used(), 10);
        m.begin_batch();
        m.charge(95);
        assert_eq!(m.used(), 10, "a stop test inside a batch sees the count at its start");
        m.end_batch();
        assert_eq!(m.used(), 105);
        assert_eq!(m.left(), Some(0));
    }

    #[test]
    fn tally_charges_every_way_out() {
        let m = WorkMeter::default();
        {
            let mut t = Tally::new(&m, 3, 4);
            for _ in 0..9 {
                t.tick();
            }
            assert_eq!(m.used(), 24);
        }
        assert_eq!(m.used(), 27);
    }

    #[test]
    fn limits_follow_the_work_left_at_their_start() {
        let m = WorkMeter::limited(1000);
        m.charge(200);
        let l = Limit::share(None, Instant::now(), &m, 200, 0.5);
        assert!(!l.passed());
        m.charge(400);
        assert!(!l.before(Span::default()).passed(), "exactly at the limit is not past it");
        m.charge(1);
        assert!(l.passed());
        let cloned = m.clone();
        assert_eq!(cloned.used(), 601, "clones share one count");
    }
}
