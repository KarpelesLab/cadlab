//! Oracle tests (docs/TESTING.md). Real comparisons arrive with the KiCad writers (M2+).

mod common;

use common::oracle::{Oracle, enabled, require};

#[test]
fn oracle_harness_skips_when_disabled() {
    if !enabled() {
        assert_eq!(require(Oracle::KicadCli), None);
    }
}
