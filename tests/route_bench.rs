//! Router benchmark suite (`tests/common/routebench.rs`): every board routed with zero DRC
//! errors. `#[ignore]`d (minutes in debug builds):
//! `cargo test --release --test route_bench -- --ignored --nocapture`, or
//! `cargo run --release --example route_bench` for the table alone.

#[path = "common/routebench.rs"]
mod routebench;

#[test]
#[ignore]
fn route_benchmark_suite() {
    let render = std::env::var_os("CADLAB_ROUTE_RENDER").map(std::path::PathBuf::from);
    let fr = routebench::freerouting_tool();
    eprintln!("{}", routebench::HEADER);
    for case in routebench::cases() {
        let m = routebench::run_cadlab(&case, 120_000, render.as_deref());
        eprintln!("{}", routebench::row(&case, &m));
        assert_eq!(m.drc_errors, 0, "{}", case.name);
        if let Some(tool) = fr.as_ref().filter(|_| case.freerouting) {
            eprintln!("{}", routebench::row(&case, &routebench::run_freerouting(&case, tool)));
        }
    }
}
