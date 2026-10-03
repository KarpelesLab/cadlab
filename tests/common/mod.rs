//! Test helpers shared by cadlab's integration tests (`mod common;` in each test file).
//!
//! - [`oracle`]: locate external verification tools (KiCad, freerouting, gerbv, ...). Oracle
//!   tests run only when `CADLAB_ORACLES=1`; they never link or vendor those tools
//!   (docs/TESTING.md, DECISIONS D7).
//! - [`golden`]: compare output with checked-in golden files.
//! - [`boards`]: example boards built through the command registry.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code, missing_docs)]

pub mod oracle {
    //! External verification oracles.

    use std::path::PathBuf;
    use std::process::Command;

    /// Environment variable enabling oracle tests.
    pub const ENABLE_VAR: &str = "CADLAB_ORACLES";

    /// A known oracle tool.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Oracle {
        /// `kicad-cli`: DRC, ERC, Gerber export of KiCad files we write.
        KicadCli,
        /// freerouting: router benchmarks on Specctra DSN.
        Freerouting,
        /// gerbv: Gerber parsing and rasterization.
        Gerbv,
        /// ngspice: SPICE netlist checks.
        Ngspice,
    }

    impl Oracle {
        /// Executable name, overridable with `CADLAB_ORACLE_<NAME>=/path/to/tool`.
        pub fn program(self) -> (&'static str, &'static str) {
            match self {
                Oracle::KicadCli => ("kicad-cli", "CADLAB_ORACLE_KICAD_CLI"),
                Oracle::Freerouting => ("freerouting", "CADLAB_ORACLE_FREEROUTING"),
                Oracle::Gerbv => ("gerbv", "CADLAB_ORACLE_GERBV"),
                Oracle::Ngspice => ("ngspice", "CADLAB_ORACLE_NGSPICE"),
            }
        }
    }

    /// Whether oracle tests are enabled.
    pub fn enabled() -> bool {
        std::env::var(ENABLE_VAR).is_ok_and(|v| v == "1")
    }

    /// Path of the oracle executable, or `None` (with a message on stderr) if oracle tests are
    /// disabled or the tool is missing. When oracles are enabled, a missing tool panics so CI
    /// never silently skips.
    pub fn require(o: Oracle) -> Option<PathBuf> {
        if !enabled() {
            eprintln!("skipping: oracle tests disabled (set {ENABLE_VAR}=1)");
            return None;
        }
        let (name, var) = o.program();
        let path = std::env::var_os(var).map(PathBuf::from).or_else(|| find_in_path(name));
        match path {
            Some(p) => Some(p),
            None => {
                panic!("{ENABLE_VAR}=1 but oracle `{name}` was not found (install it or set {var})")
            }
        }
    }

    fn find_in_path(name: &str) -> Option<PathBuf> {
        let paths = std::env::var_os("PATH")?;
        std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file())
    }

    /// Runs an oracle and returns stdout, panicking with stderr on failure.
    pub fn run(program: &std::path::Path, args: &[&str]) -> String {
        let out =
            Command::new(program).args(args).output().unwrap_or_else(|e| panic!("running {}: {e}", program.display()));
        assert!(
            out.status.success(),
            "{} {:?} failed:\n{}",
            program.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

pub mod golden {
    //! Golden files: expected outputs checked into `tests/golden/`. Set `CADLAB_BLESS=1` to
    //! (re)write them from the current output.

    use std::path::Path;

    /// Asserts that `actual` equals the content of `path`, or writes it when blessing.
    pub fn assert_golden(path: &Path, actual: &str) {
        if std::env::var("CADLAB_BLESS").is_ok_and(|v| v == "1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, actual).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{}: {e} (run with CADLAB_BLESS=1 to create it)", path.display()));
        assert!(
            expected == actual,
            "{} differs from the current output (CADLAB_BLESS=1 to update):\n--- expected\n{expected}\n--- actual\n{actual}",
            path.display()
        );
    }

    /// Compares every file under `dir` with the golden directory `golden`, recursively,
    /// ignoring paths that start with any of `skip`.
    pub fn assert_golden_dir(dir: &Path, golden: &Path, skip: &[&str]) {
        let mut files = Vec::new();
        collect(dir, dir, skip, &mut files);
        files.sort();
        for rel in &files {
            let actual = std::fs::read_to_string(dir.join(rel)).unwrap();
            assert_golden(&golden.join(rel), &actual);
        }
        if std::env::var("CADLAB_BLESS").is_err() {
            let mut expected = Vec::new();
            collect(golden, golden, &[], &mut expected);
            expected.sort();
            assert_eq!(files, expected, "file set differs from {}", golden.display());
        }
    }

    fn collect(root: &Path, dir: &Path, skip: &[&str], out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if skip.iter().any(|s| rel.starts_with(s)) {
                continue;
            }
            if p.is_dir() {
                collect(root, &p, skip, out);
            } else {
                out.push(rel);
            }
        }
    }
}

pub mod boards {
    //! Example boards built through the command registry.

    use cadlab::command::{Registry, RunOptions, Session, Step};
    use serde_json::{Value, json};

    pub fn pins(list: &[(&str, &str, &str)]) -> Value {
        Value::Array(list.iter().map(|(n, name, kind)| json!({"number": n, "name": name, "kind": kind})).collect())
    }

    /// Parts and components of a small ATtiny85 board.
    pub fn build_board(r: &Registry, s: &mut Session) {
        let steps = vec![
            json!({"cmd": "part.create", "args": {"id": "PWR_HDR", "category": "connector", "description": "2-pin power header",
                "package": "PinHeader 1x02", "pins": pins(&[("1", "VBUS", "passive"), ("2", "GND", "passive")])}}),
            json!({"cmd": "part.create", "args": {"category": "ldo", "manufacturer": "Diodes", "mpn": "AP2112K-3.3TRG1", "package": "SOT-23-5",
                "pins": pins(&[("1", "VIN", "power_in"), ("2", "GND", "power_in"), ("3", "EN", "input"), ("4", "NC", "no_connect"), ("5", "VOUT", "power_out")])}}),
            json!({"cmd": "part.create", "args": {"category": "mcu", "manufacturer": "Microchip", "mpn": "ATTINY85-20SU", "package": "SOIC-8",
                "pins": pins(&[("1", "PB5", "bidirectional"), ("2", "PB3", "bidirectional"), ("3", "PB4", "bidirectional"), ("4", "GND", "power_in"),
                               ("5", "PB0", "bidirectional"), ("6", "PB1", "bidirectional"), ("7", "PB2", "bidirectional"), ("8", "VCC", "power_in")])}}),
            json!({"cmd": "part.create", "args": {"id": "ISP_HDR", "category": "connector", "description": "AVR ISP 2x3 header", "package": "PinHeader 2x03",
                "pins": pins(&[("1", "MISO", "passive"), ("2", "VCC", "passive"), ("3", "SCK", "passive"), ("4", "MOSI", "passive"), ("5", "RESET", "passive"), ("6", "GND", "passive")])}}),
            json!({"cmd": "circuit.add", "args": {"part": "PWR_HDR", "refdes": "J1"}}),
            json!({"cmd": "circuit.add", "args": {"part": "AP2112K-3.3TRG1"}}),
            json!({"cmd": "circuit.add", "args": {"part": "ATTINY85-20SU"}}),
            json!({"cmd": "circuit.add", "args": {"part": "ISP_HDR", "refdes": "J2"}}),
            json!({"cmd": "circuit.add", "args": {"part": "C 1uF 16V X5R 0402", "count": 2}}),
            json!({"cmd": "circuit.add", "args": {"part": "C 100nF 16V X7R 0402"}}),
            json!({"cmd": "circuit.add", "args": {"part": "R 10k 1% 0402"}}),
            json!({"cmd": "circuit.add", "args": {"part": "R 1k 1% 0402"}}),
            json!({"cmd": "circuit.add", "args": {"part": "LED red 0603"}}),
            json!({"cmd": "net.connect", "args": {"net": "VBUS", "pins": ["J1.VBUS", "U1.VIN", "U1.EN", "C1.1"]}}),
            json!({"cmd": "net.connect", "args": {"net": "GND", "pins": ["J1.GND", "U1.GND", "C1.2", "C2.2", "C3.2", "U2.GND", "D1.K", "J2.GND"]}}),
            json!({"cmd": "net.connect", "args": {"net": "3V3", "pins": ["U1.VOUT", "C2.1", "C3.1", "U2.VCC", "R1.1", "J2.VCC"]}}),
            json!({"cmd": "net.connect", "args": {"net": "RESET", "pins": ["U2.PB5", "R1.2", "J2.RESET"]}}),
            json!({"cmd": "net.connect", "args": {"net": "MOSI", "pins": ["U2.PB0", "J2.MOSI"]}}),
            json!({"cmd": "net.connect", "args": {"net": "MISO", "pins": ["U2.PB1", "J2.MISO"]}}),
            json!({"cmd": "net.connect", "args": {"net": "SCK", "pins": ["U2.PB2", "J2.SCK"]}}),
            json!({"cmd": "net.connect", "args": {"net": "LED_DRIVE", "pins": ["U2.PB3", "R2.1"]}}),
            json!({"cmd": "net.connect", "args": {"net": "LED_A", "pins": ["R2.2", "D1.A"]}}),
        ];
        let steps: Vec<Step> = serde_json::from_value(Value::Array(steps)).unwrap();
        r.execute_batch(s, steps, RunOptions::default()).unwrap_or_else(|f| panic!("step {:?}: {}", f.step, f.error));
    }
}
