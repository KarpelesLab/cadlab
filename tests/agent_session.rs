//! The agent-ergonomics session (docs/TESTING.md, "Agent sessions"): a board designed from a
//! prompt with nothing but the `cadlab` CLI, replayed as a regression test.
//!
//! Prompt: "Design a 2-layer 50x40 mm board: ATmega328P or STM32 MCU, USB-C power input with ESD
//! protection, 3.3 V LDO, crystal, SWD/ISP header, two status LEDs, I2C header with pull-ups.
//! Generic parts are fine where no supplier is configured. Place, autoroute, pass ERC/lint and
//! DRC, render schematic and board, and export a JLCPCB fab bundle."
//!
//! Every step is a command line exactly as an agent types it, using the forms the friction log
//! asked for (`cadlab erc`, `cadlab drc`, `--at 25mm,33.5mm`, `--budget 60s`, `--pins @file`).
//! A step that fails, or a check that is not clean at the end, fails the test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent_session")
}

/// Runs the binary isolated from the user's configuration: no suppliers, like a fresh machine.
fn cadlab(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cadlab"))
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", cwd.join(".no-config"))
        .env("XDG_CACHE_HOME", cwd.join(".no-cache"))
        .env("XDG_DATA_HOME", cwd.join(".no-data"))
        .env("HOME", cwd.join(".no-home"))
        .env_remove("CADLAB_CATALOGS")
        .env_remove("DIGIKEY_CLIENT_ID")
        .env_remove("DIGIKEY_CLIENT_SECRET")
        .env_remove("MOUSER_API_KEY")
        .env_remove("NEXAR_CLIENT_ID")
        .env_remove("NEXAR_CLIENT_SECRET")
        .args(args)
        .output()
        .unwrap()
}

/// Runs one step; it must succeed (exit 0).
fn step(cwd: &Path, args: &[&str]) -> Output {
    let o = cadlab(cwd, args);
    assert!(
        o.status.success(),
        "`cadlab {}` exited with {:?}\nstdout: {}\nstderr: {}",
        args.join(" "),
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

/// Runs a step with `--json` and returns the document.
fn step_json(cwd: &Path, args: &[&str]) -> Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    let o = step(cwd, &a);
    serde_json::from_slice(&o.stdout).unwrap()
}

fn codes(v: &Value) -> Vec<String> {
    v["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap().to_string()).collect()
}

const PIN_ISP: &str = r#"[{"number":"1","name":"MISO"},{"number":"2","name":"VCC"},{"number":"3","name":"SCK"},{"number":"4","name":"MOSI"},{"number":"5","name":"RESET"},{"number":"6","name":"GND"}]"#;
const PIN_I2C: &str = r#"[{"number":"1","name":"VCC"},{"number":"2","name":"GND"},{"number":"3","name":"SCL"},{"number":"4","name":"SDA"}]"#;
const PIN_LDO: &str = r#"[{"number":"1","name":"VIN","kind":"power_in"},{"number":"2","name":"GND","kind":"power_in"},{"number":"3","name":"EN","kind":"input"},{"number":"4","name":"NC","kind":"no_connect"},{"number":"5","name":"VOUT","kind":"power_out"}]"#;
const PIN_TVS: &str = r#"[{"number":"1","name":"K"},{"number":"2","name":"A"}]"#;
const PIN_XTAL: &str = r#"[{"number":"1","name":"X1"},{"number":"2","name":"GND"},{"number":"3","name":"X2"},{"number":"4","name":"GND"}]"#;
// Both VBUS and both GND contacts are power outputs: one driver per net for ERC (same
// component, same pin name), which used to be reported as an output conflict.
const PIN_USBC: &str = r#"[{"number":"A5","name":"CC1"},{"number":"B5","name":"CC2"},{"number":"A9","name":"VBUS","kind":"power_out"},{"number":"B9","name":"VBUS","kind":"power_out"},{"number":"A12","name":"GND","kind":"power_out"},{"number":"B12","name":"GND","kind":"power_out"}]"#;

#[test]
fn atmega_usb_c_board_from_a_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let pins = fixtures().join("atmega328p_pins.json");
    let usbc = fixtures().join("USB_C_6P.kicad_mod");
    let pins_arg = format!("@{}", pins.display());
    let usbc = usbc.to_str().unwrap();

    step(root, &["project", "new", "demo", "--targets", "jlcpcb"]);
    let p = root.join("demo");
    let p = p.as_path();

    // Parts: no supplier is configured, so parts are created from pin lists and generic specs.
    #[rustfmt::skip]
    let parts: &[&[&str]] = &[
        &["part", "create", "--category", "mcu", "--mpn", "ATMEGA328P-AU", "--manufacturer", "Microchip",
          "--description", "8-bit AVR MCU, 32KB flash", "--package", "TQFP-32", "--pins", &pins_arg],
        &["part", "create", "--category", "ldo", "--mpn", "AP2112K-3.3TRG1", "--manufacturer", "Diodes",
          "--description", "LDO 3.3V 600mA", "--package", "SOT-23-5", "--params", "voltage_out=3.3V",
          "--params", "current_out=600mA", "--pins", PIN_LDO],
        &["part", "create", "--category", "diode", "--mpn", "SMF5.0A", "--description", "TVS 5V unidirectional",
          "--package", "SOD-123F", "--pins", PIN_TVS],
        &["part", "create", "--category", "connector", "--id", "ISP_2x3", "--description", "AVR ISP header 2x3",
          "--package", "PinHeader 2x03", "--pins", PIN_ISP],
        &["part", "create", "--category", "connector", "--id", "I2C_1x4", "--description", "I2C header",
          "--package", "PinHeader 1x04", "--pins", PIN_I2C],
        &["part", "create", "--category", "crystal", "--id", "XTAL_8MHz_3225", "--description", "8 MHz crystal 3.2x2.5mm",
          "--params", "frequency=8MHz", "--package", "DFN-4 3.2x2.5mm P1.6mm", "--pins", PIN_XTAL],
        // No generator covers USB-C: import a hand-written land pattern, then use it.
        &["footprint", "import-kicad", usbc, "--license", "own work"],
        &["part", "create", "--category", "connector", "--mpn", "USB-C-6P", "--description",
          "USB-C receptacle, power only (6 pin)", "--footprint", "USB_C_6P", "--pins", PIN_USBC],
    ];
    for a in parts {
        step(p, a);
    }

    // Components.
    #[rustfmt::skip]
    let components: &[&[&str]] = &[
        &["circuit", "add", "USB-C-6P", "--refdes", "J1"],
        &["circuit", "add", "SMF5.0A", "--refdes", "D1"],
        &["circuit", "add", "AP2112K-3.3TRG1", "--refdes", "U1"],
        &["circuit", "add", "ATMEGA328P-AU", "--refdes", "U2"],
        &["circuit", "add", "XTAL_8MHz_3225", "--refdes", "Y1"],
        &["circuit", "add", "ISP_2x3", "--refdes", "J2"],
        &["circuit", "add", "I2C_1x4", "--refdes", "J3"],
        &["circuit", "add", "R 5.1k 1% 0402", "--count", "2"],      // R1, R2: CC pull-downs
        &["circuit", "add", "R 10k 1% 0402"],                       // R3: reset pull-up
        &["circuit", "add", "R 4.7k 1% 0402", "--count", "2"],      // R4, R5: I2C pull-ups
        &["circuit", "add", "R 1k 1% 0402", "--count", "2"],        // R6, R7: LED resistors
        &["circuit", "add", "C 1uF 16V X7R 0402", "--count", "2"],  // C1, C2: LDO in/out
        &["circuit", "add", "C 100nF 16V X7R 0402", "--count", "4"],// C3..C5 decoupling, C6 AREF
        &["circuit", "add", "C 22pF 50V C0G 0402", "--count", "2"], // C7, C8: crystal loads
        &["circuit", "add", "LED green 0603"],                      // D2
        &["circuit", "add", "LED red 0603"],                        // D3
    ];
    for a in components {
        step(p, a);
    }

    // Nets.
    #[rustfmt::skip]
    let nets: &[&[&str]] = &[
        &["net", "connect", "VBUS", "J1.VBUS", "D1.K", "U1.VIN", "U1.EN", "C1.1"],
        &["net", "connect", "GND", "J1.GND", "D1.A", "U1.GND", "C1.2", "C2.2", "U2.GND", "C3.2", "C4.2", "C5.2",
          "C6.2", "C7.2", "C8.2", "Y1.GND", "R1.2", "R2.2", "J2.GND", "J3.GND", "D2.K", "D3.K"],
        &["net", "connect", "CC1", "J1.CC1", "R1.1"],
        &["net", "connect", "CC2", "J1.CC2", "R2.1"],
        &["net", "connect", "+3V3", "U1.VOUT", "C2.1", "U2.VCC", "U2.AVCC", "C3.1", "C4.1", "C5.1", "R3.1", "R4.1",
          "R5.1", "J2.VCC", "J3.VCC"],
        &["net", "connect", "AREF", "U2.AREF", "C6.1"],
        &["net", "connect", "XTAL1", "U2.XTAL1", "Y1.X1", "C7.1"],
        &["net", "connect", "XTAL2", "U2.XTAL2", "Y1.X2", "C8.1"],
        &["net", "connect", "RESET", "U2.RESET", "R3.2", "J2.RESET"],
        &["net", "connect", "MOSI", "U2.MOSI", "J2.MOSI"],
        &["net", "connect", "MISO", "U2.MISO", "J2.MISO"],
        &["net", "connect", "SCK", "U2.SCK", "J2.SCK"],
        &["net", "connect", "SDA", "U2.SDA", "R5.2", "J3.SDA"],
        &["net", "connect", "SCL", "U2.SCL", "R4.2", "J3.SCL"],
        &["net", "connect", "LED_G", "U2.PD6", "R6.1"],
        &["net", "connect", "LED_R", "U2.PD7", "R7.1"],
        &["net", "connect", "LED_G_K", "R6.2", "D2.A"],
        &["net", "connect", "LED_R_K", "R7.2", "D3.A"],
        &["net", "no-connect", "U1.NC", "U2.PD3..PD5", "U2.PB0..PB2", "U2.ADC6", "U2.ADC7", "U2.PC0..PC3",
          "U2.PD0..PD2"],
    ];
    for a in nets {
        step(p, a);
    }

    // ERC and lint, through the top-level shortcuts.
    let erc = step_json(p, &["erc"]);
    assert_eq!(erc["output"]["errors"], 0, "{:?}", codes(&erc));
    assert_eq!(erc["output"]["warnings"], 0, "{:?}", codes(&erc));
    let lint = step_json(p, &["lint"]);
    assert!(codes(&lint).is_empty(), "{:?}", codes(&lint));

    // Board.
    #[rustfmt::skip]
    let board: &[&[&str]] = &[
        &["board", "setup", "--layers", "2", "--thickness", "1.6mm"],
        &["board", "outline", "--width", "50mm", "--height", "40mm", "--corner-radius", "1mm"],
        &["board", "rules", "--fab", "jlcpcb"],
        &["place", "set", "J1", "--at", "25mm,33.5mm", "--rotation", "180"],
        &["place", "lock", "J1"],
        &["place", "auto"],
        &["route", "all", "--budget", "60s"],
    ];
    for a in board {
        step(p, a);
    }
    let status = step_json(p, &["route", "status"]);
    assert_eq!(status["output"]["unrouted"], 0, "{status}");

    // DRC through `cadlab drc` alone: clean, warnings included (the generated footprints used to
    // trip the fab-derived silk clearance).
    let drc = step_json(p, &["drc"]);
    assert_eq!(drc["output"]["errors"], 0, "{:?}", codes(&drc));
    assert_eq!(drc["output"]["warnings"], 0, "{:?}", codes(&drc));

    // Renders: output paths are positional for every render command.
    step(p, &["render", "schematic", "out/schematic.svg"]);
    step(p, &["render", "board", "out/board.png"]);
    step(p, &["render", "footprint", "USB_C_6P", "out/usbc.svg"]);
    for f in ["schematic.svg", "board.png", "usbc.svg"] {
        assert!(p.join("out").join(f).is_file(), "{f}");
    }

    // JLCPCB bundle.
    let fab = step_json(p, &["fab", "export", "jlcpcb"]);
    assert_eq!(codes(&fab), ["fab.no_suppliers"], "only the expected supplier warning");
    let dir = p.join("out/fab/jlcpcb");
    for f in ["demo.GTL", "demo.GBL", "demo.GKO", "demo-PTH.drl", "demo-BOM-jlcpcb.csv", "demo-CPL-jlcpcb.csv"] {
        assert!(dir.join(f).is_file(), "{f} missing");
    }
    assert!(dir.join("fab-lock.json").is_file());
    let cpl = std::fs::read_to_string(dir.join("demo-CPL-jlcpcb.csv")).unwrap();
    assert_eq!(cpl.lines().count(), 1 + 24, "header + 24 placed components:\n{cpl}");
}
