//! Agent session 3 (docs/TESTING.md, "Agent sessions"): a 4-layer USB device designed with
//! nothing but the `cadlab` CLI, replayed as a regression test.
//!
//! Prompt: "4-layer 60x45 mm board: STM32 (LQFP-48) with USB full-speed device on USB-C (D+/D-
//! as a 90 Ω differential pair with ESD), SPI NOR flash, 3.3 V buck converter from 5 V with its
//! inductor and caps, SWD header, 8 MHz crystal, reset button, two LEDs. Use a JLCPCB 4-layer
//! stackup, solve the USB pair impedance, place, route (pair first), length-match D+/D-, pass
//! ERC/lint/DRC, render, export JLCPCB bundle and STEP."
//!
//! Every step is a command line as the agent typed it, after the session's friction was fixed:
//! the fab's stackup in one command (`board fab-stackup`), the USB-C land pattern with pads in
//! datasheet coordinates and an offset body, the shield pin added with `part create --replace`
//! as the warning suggests, ERC/lint that see the buck output behind its inductor, placement
//! hints (`place hint`) for the connector and the LEDs, `--outline board` as a bare word, the
//! pair routed coupled end to end, and the stackup to order recorded in `fab-lock.json`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent_session3")
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

fn mm(v: &Value) -> f64 {
    v.as_str().unwrap().trim_end_matches("mm").parse::<f64>().unwrap()
}

const PIN_ESD: &str = r#"[{"number":"1","name":"IO1","kind":"passive"},{"number":"2","name":"GND","kind":"power_in"},{"number":"3","name":"IO2","kind":"passive"},{"number":"4","name":"IO2","kind":"passive"},{"number":"5","name":"VBUS","kind":"power_in"},{"number":"6","name":"IO1","kind":"passive"}]"#;
const PIN_FLASH: &str = r#"[{"number":"1","name":"CS","kind":"input"},{"number":"2","name":"DO","kind":"output"},{"number":"3","name":"WP","kind":"input"},{"number":"4","name":"GND","kind":"power_in"},{"number":"5","name":"DI","kind":"input"},{"number":"6","name":"CLK","kind":"input"},{"number":"7","name":"HOLD","kind":"input"},{"number":"8","name":"VCC","kind":"power_in"}]"#;
const PIN_BUCK: &str = r#"[{"number":"1","name":"EN","kind":"input"},{"number":"2","name":"GND","kind":"power_in"},{"number":"3","name":"LX","kind":"power_out"},{"number":"4","name":"IN","kind":"power_in"},{"number":"5","name":"FB","kind":"input"}]"#;
const PIN_XTAL: &str = r#"[{"number":"1","name":"X1"},{"number":"2","name":"GND"},{"number":"3","name":"X2"},{"number":"4","name":"GND"}]"#;
const PIN_SWD: &str = r#"[{"number":"1","name":"VTREF"},{"number":"2","name":"SWDIO"},{"number":"3","name":"GND"},{"number":"4","name":"SWCLK"},{"number":"5","name":"GND"},{"number":"6","name":"SWO"},{"number":"7","name":"KEY"},{"number":"8","name":"NC"},{"number":"9","name":"GND"},{"number":"10","name":"NRST"}]"#;
const PADS_SW: &str = r#"[{"number":"1","at":["-3mm","1.85mm"],"size":["1mm","0.75mm"],"shape":"rect"},{"number":"1","at":["3mm","1.85mm"],"size":["1mm","0.75mm"],"shape":"rect"},{"number":"2","at":["-3mm","-1.85mm"],"size":["1mm","0.75mm"],"shape":"rect"},{"number":"2","at":["3mm","-1.85mm"],"size":["1mm","0.75mm"],"shape":"rect"}]"#;
const PIN_SW: &str = r#"[{"number":"1","name":"A","kind":"passive"},{"number":"2","name":"B","kind":"passive"}]"#;
// Datasheet origin at the contact row; the body (and the plug opening) reaches 7 mm in front.
const USBC_BODY: &str = r#"{"width":"8.94mm","length":"7.3mm","height":"3.2mm","at":["0mm","-3.4mm"]}"#;

#[test]
fn stm32_usb_device_four_layers_from_a_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let at = |name: &str| format!("@{}", fixtures().join(name).display());
    let (mcu_pins, usbc_pads, usbc_pins) =
        (at("stm32f103c8_pins.json"), at("usb_c_16p_pads.json"), at("usb_c_16p_pins.json"));

    step(root, &["project", "new", "usbdev", "--targets", "jlcpcb"]);
    let p = root.join("usbdev");
    let p = p.as_path();

    // The fab's impedance stackups listed, then one applied in one step (session 3 set three
    // dielectrics by hand, and the fab check then rejected the stackup page's 15.2 µm inner
    // copper).
    let list = step_json(p, &["board", "fab-stackup", "jlcpcb"]);
    assert!(list["output"]["stackups"].as_array().unwrap().iter().any(|x| x["id"] == "JLC04161H-7628"), "{list}");
    let st = step_json(p, &["board", "fab-stackup", "jlcpcb", "JLC04161H-7628"]);
    assert_eq!(st["output"]["copper"].as_array().unwrap().len(), 4, "{st}");
    assert_eq!(st["output"]["dielectrics"][0]["thickness"], "0.2104mm", "{st}");
    assert!(codes(&st).is_empty(), "{:?}", codes(&st));

    #[rustfmt::skip]
    let parts: &[&[&str]] = &[
        &["part", "create", "--category", "mcu", "--mpn", "STM32F103C8T6", "--manufacturer", "ST",
          "--description", "Cortex-M3 MCU 64KB flash, USB FS", "--package", "LQFP-48", "--pins", &mcu_pins],
        &["part", "create", "--category", "ic", "--mpn", "USBLC6-2SC6", "--manufacturer", "ST",
          "--description", "USB ESD protection, 2 lines", "--package", "SOT-23-6", "--pins", PIN_ESD],
        &["part", "create", "--category", "ic", "--mpn", "W25Q32JVSSIQ", "--manufacturer", "Winbond",
          "--description", "32Mbit SPI NOR flash", "--package", "SOIC-8", "--pins", PIN_FLASH],
        &["part", "create", "--category", "regulator", "--mpn", "SY8089AAAC", "--manufacturer", "Silergy",
          "--description", "Buck 2A 1MHz, 2.7-5.5V in", "--package", "SOT-23-5", "--pins", PIN_BUCK],
        // Pads in the datasheet's coordinates, the body offset to where it is.
        &["footprint", "create", "USB_C_16P", "--pads", &usbc_pads, "--body", USBC_BODY,
          "--description", "USB-C 2.0 receptacle, 16 pin, mid-mount SMD + THT shell"],
        &["part", "create", "--category", "crystal", "--id", "XTAL_8MHz_3225", "--description",
          "8 MHz crystal 3.2x2.5mm, 20pF load", "--params", "frequency=8MHz", "--package",
          "DFN-4 3.2x2.5mm P1.6mm", "--pins", PIN_XTAL],
        &["part", "create", "--category", "connector", "--id", "SWD_2x05_1.27", "--description",
          "Cortex debug header 2x5 1.27mm", "--package", "PinHeader 2x05 P1.27mm", "--pins", PIN_SWD],
        &["footprint", "create", "SW_TACT_5.1x5.1", "--pads", PADS_SW, "--body",
          r#"{"width":"5.1mm","length":"5.1mm","height":"1.5mm"}"#, "--description", "SMD tactile switch 5.1x5.1mm"],
        &["part", "create", "--category", "switch", "--id", "SW_RESET", "--description", "Tactile switch, reset",
          "--footprint", "SW_TACT_5.1x5.1", "--pins", PIN_SW],
        &["part", "generic", "L 2.2uH 2A 1210"],
    ];
    for a in parts {
        step(p, a);
    }
    let shown = String::from_utf8(step(p, &["footprint", "show", "USB_C_16P"]).stdout).unwrap();
    assert!(shown.contains("body 8.94mm x 7.3mm x 3.2mm at (0mm, -3.4mm)"), "{shown}");

    // The USB-C part without its shell: one warning saying how to add a shield pin; the agent
    // follows it with `--replace`.
    let usbc = |pins: &str, replace: bool| {
        let mut a = vec![
            "part",
            "create",
            "--category",
            "connector",
            "--mpn",
            "TYPE-C-31-M-12",
            "--manufacturer",
            "HRO",
            "--description",
            "USB-C 2.0 receptacle 16P",
            "--footprint",
            "USB_C_16P",
            "--pins",
        ];
        a.push(pins);
        if replace {
            a.push("--replace");
        }
        step_json(p, &a)
    };
    let first = usbc(&usbc_pins, false);
    assert_eq!(codes(&first), ["part.unmapped_shield"], "{first}");
    let hint = first["diagnostics"][0]["hint"].as_str().unwrap();
    assert!(hint.contains(r#"{"number": "S1", "name": "SHIELD", "kind": "passive"}"#), "{hint}");
    let mut pins: Value =
        serde_json::from_str(&std::fs::read_to_string(fixtures().join("usb_c_16p_pins.json")).unwrap()).unwrap();
    pins.as_array_mut().unwrap().push(serde_json::json!({"number": "S1", "name": "SHIELD", "kind": "passive"}));
    let with_shield = root.join("usbc_pins_shield.json");
    std::fs::write(&with_shield, pins.to_string()).unwrap();
    let second = usbc(&format!("@{}", with_shield.display()), true);
    assert!(codes(&second).is_empty(), "{:?}", codes(&second));

    #[rustfmt::skip]
    let components: &[&[&str]] = &[
        &["circuit", "add", "TYPE-C-31-M-12", "--refdes", "J1"],
        &["circuit", "add", "USBLC6-2SC6", "--refdes", "U1"],
        &["circuit", "add", "SY8089AAAC", "--refdes", "U2"],
        &["circuit", "add", "STM32F103C8T6", "--refdes", "U3"],
        &["circuit", "add", "W25Q32JVSSIQ", "--refdes", "U4"],
        &["circuit", "add", "XTAL_8MHz_3225", "--refdes", "Y1"],
        &["circuit", "add", "SWD_2x05_1.27", "--refdes", "J2"],
        &["circuit", "add", "SW_RESET", "--refdes", "SW1"],
        &["circuit", "add", "L_2.2uH_2A_1210", "--refdes", "L1"],
        &["circuit", "add", "LED green 0603", "--refdes", "D1"],
        &["circuit", "add", "LED red 0603", "--refdes", "D2"],
        &["circuit", "add", "R 5.1k 1% 0402", "--count", "2"],      // R1, R2: CC pull-downs
        &["circuit", "add", "R 1.5k 1% 0402"],                      // R3: D+ pull-up
        &["circuit", "add", "R 10k 1% 0402"],                       // R4: BOOT0 pull-down
        &["circuit", "add", "R 100k 1% 0402"],                      // R5, R6: feedback divider
        &["circuit", "add", "R 22k 1% 0402"],
        &["circuit", "add", "R 1k 1% 0402", "--count", "2"],        // R7, R8: LEDs
        &["circuit", "add", "C 10uF 10V X5R 0805"],                 // C1: buck input
        &["circuit", "add", "C 22uF 10V X5R 0805"],                 // C2: buck output
        &["circuit", "add", "C 20pF 50V C0G 0402", "--count", "2"], // C3, C4: crystal loads
        &["circuit", "add", "C 100nF 16V X7R 0402", "--count", "4"],// C5..C8: MCU decoupling
        &["circuit", "add", "C 1uF 16V X7R 0402"],                  // C9: VDDA
        &["circuit", "add", "C 100nF 16V X7R 0402", "--count", "2"],// C10 flash, C11 NRST
    ];
    for a in components {
        step(p, a);
    }

    #[rustfmt::skip]
    let nets: &[&[&str]] = &[
        &["net", "connect", "VBUS", "J1.VBUS", "U1.VBUS", "U2.IN", "U2.EN", "C1.1"],
        &["net", "connect", "GND", "J1.GND", "J1.SHIELD", "U1.GND", "U2.GND", "C1.2", "C2.2", "R6.2", "U3.VSS",
          "U3.VSSA", "C3.2", "C4.2", "C5.2", "C6.2", "C7.2", "C8.2", "C9.2", "C10.2", "C11.2", "Y1.GND", "U4.GND",
          "J2.GND", "R1.2", "R2.2", "R4.2", "SW1.B", "D1.K", "D2.K"],
        &["net", "connect", "SW", "U2.LX", "L1.1"],
        &["net", "connect", "+3V3", "L1.2", "C2.1", "R5.1", "U3.VDD", "U3.VDDA", "U3.VBAT", "C5.1", "C6.1", "C7.1",
          "C8.1", "C9.1", "U4.VCC", "U4.WP", "U4.HOLD", "C10.1", "R3.1", "J2.VTREF"],
        &["net", "connect", "FB", "U2.FB", "R5.2", "R6.1"],
        &["net", "connect", "USB_DP", "J1.DP1", "J1.DP2", "U1.IO1", "U3.PA12", "R3.2"],
        &["net", "connect", "USB_DN", "J1.DN1", "J1.DN2", "U1.IO2", "U3.PA11"],
        &["net", "connect", "CC1", "J1.CC1", "R1.1"],
        &["net", "connect", "CC2", "J1.CC2", "R2.1"],
        &["net", "connect", "OSC_IN", "U3.OSC_IN", "Y1.X1", "C3.1"],
        &["net", "connect", "OSC_OUT", "U3.OSC_OUT", "Y1.X2", "C4.1"],
        &["net", "connect", "NRST", "U3.NRST", "SW1.A", "C11.1", "J2.NRST"],
        &["net", "connect", "BOOT0", "U3.BOOT0", "R4.1"],
        &["net", "connect", "SWDIO", "U3.PA13", "J2.SWDIO"],
        &["net", "connect", "SWCLK", "U3.PA14", "J2.SWCLK"],
        &["net", "connect", "SWO", "U3.PB3", "J2.SWO"],
        &["net", "connect", "FLASH_CS", "U3.PA4", "U4.CS"],
        &["net", "connect", "FLASH_SCK", "U3.PA5", "U4.CLK"],
        &["net", "connect", "FLASH_MISO", "U3.PA6", "U4.DO"],
        &["net", "connect", "FLASH_MOSI", "U3.PA7", "U4.DI"],
        &["net", "connect", "LED1", "U3.PB12", "R7.1"],
        &["net", "connect", "LED2", "U3.PB13", "R8.1"],
        &["net", "connect", "LED1_A", "R7.2", "D1.A"],
        &["net", "connect", "LED2_A", "R8.2", "D2.A"],
        &["net", "no-connect", "J1.SBU1", "J1.SBU2", "J2.KEY", "J2.NC", "U3.PC13..PC15", "U3.PA0..PA3",
          "U3.PA8..PA10", "U3.PA15", "U3.PB0..PB2", "U3.PB4..PB11", "U3.PB14", "U3.PB15"],
    ];
    for a in nets {
        step(p, a);
    }

    // ERC sees +3V3 driven through the buck's inductor; lint looks for the output capacitor
    // behind it, not on the switch node.
    let erc = step_json(p, &["erc"]);
    assert_eq!(erc["output"]["errors"], 0, "{:?}", codes(&erc));
    assert_eq!(erc["output"]["warnings"], 0, "{:?}", codes(&erc));
    let lint = step_json(p, &["lint"]);
    assert!(codes(&lint).is_empty(), "{:?}", codes(&lint));

    // Board, impedance and the pair.
    #[rustfmt::skip]
    let board: &[&[&str]] = &[
        &["board", "outline", "--width", "60mm", "--height", "45mm", "--corner-radius", "1mm"],
        &["board", "rules", "--fab", "jlcpcb"],
        &["impedance", "solve", "90ohm", "--gap", "0.15mm", "--layer", "F.Cu", "--netclass", "USB"],
        &["diffpair", "add", "USB_DP", "USB_DN", "--name", "USB_D", "--class", "USB", "--max-skew", "0.1mm"],
        // The USB connector at the bottom edge, the LEDs and the button at the top edge.
        &["place", "hint", "J1", "--edge", "bottom"],
        &["place", "hint", "D1", "D2", "SW1", "--edge", "top"],
        &["place", "auto"],
        &["zone", "add", "GND_plane", "--net", "GND", "--layers", "In1.Cu", "--outline", "board"],
        &["zone", "add", "3V3_plane", "--net", "+3V3", "--layers", "In2.Cu", "--outline", "board"],
    ];
    for a in board {
        step(p, a);
    }
    let class = step_json(p, &["netclass", "show", "USB"]);
    let w = mm(&class["output"]["diff_pair_width"]);
    assert!((w - 0.268).abs() < 0.005, "90 Ω at 0.15 mm gap on 0.2104 mm 7628: {class}");

    let placed = step_json(p, &["place", "list"]);
    let pf = |r: &str| placed["output"]["placements"][r].clone();
    let xy = |r: &str| (mm(&pf(r)["at"][0]), mm(&pf(r)["at"][1]));
    // The receptacle at the bottom edge, opening outwards (body below its contacts: rotation 0).
    let (jx, jy) = xy("J1");
    assert!(jy < 9.0 && (0.0..60.0).contains(&jx), "J1 at ({jx}, {jy})");
    assert_eq!(pf("J1")["rotation"], "0deg", "J1 opens towards the edge: {}", pf("J1"));
    for r in ["D1", "D2", "SW1"] {
        assert!(xy(r).1 > 38.0, "{r} at the top edge: {:?}", xy(r));
    }
    // The buck's inductor at its switch pin (it went to the MCU, the largest IC on +3V3).
    let d = |a: (f64, f64), b: (f64, f64)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
    assert!(d(xy("L1"), xy("U2")) < 6.0, "L1 {:?} next to U2 {:?}", xy("L1"), xy("U2"));
    // The board is used across its width (the right third used to stay empty): parts in every
    // third, the MCU still central.
    let xs: Vec<f64> = placed["output"]["placements"].as_object().unwrap().keys().map(|r| xy(r).0).collect();
    for third in 0..3 {
        let (lo, hi) = (20.0 * third as f64, 20.0 * (third + 1) as f64);
        assert!(xs.iter().any(|x| (lo..hi).contains(x)), "nothing between x = {lo} and {hi} mm: {xs:?}");
    }
    let (ux, uy) = xy("U3");
    assert!((ux - 30.0).abs() < 6.0 && (uy - 22.5).abs() < 6.0, "MCU central: ({ux}, {uy})");

    // The pair first: coupled end to end (the D+ pull-up used to sit in its breakout, and the
    // receptacle's two contact rows were reported as a failed link).
    let dp = step_json(p, &["route", "diffpair", "USB_D", "--layers", "F.Cu"]);
    assert!(!codes(&dp).contains(&"route.diffpair_failed".to_string()), "{dp}");
    step(p, &["route", "all", "--budget", "120s"]);
    let status = step_json(p, &["route", "status"]);
    assert_eq!(status["output"]["unrouted"], 0, "{status}");
    // Lengths run along the signal path, receptacle to MCU: the D+ pull-up and the second
    // contact row are stubs, reported apart, so they are not skew (session 3's tuning added 30
    // bumps for them and left most of the pair uncoupled).
    let before = step_json(p, &["diffpair", "list"]);
    let pr = &before["output"]["pairs"][0];
    for (net, path) in [("USB_DP", &pr["path_p"]), ("USB_DN", &pr["path_n"])] {
        assert_eq!(path["net"], net, "{pr}");
        assert!(path["from"].as_str().unwrap().starts_with("J1."), "{path}");
        assert!(path["to"].as_str().unwrap().starts_with("U3."), "{path}");
        assert!(!path["stubs"].as_array().unwrap().is_empty(), "the other contact row: {path}");
    }
    let pull_up: Vec<&Value> =
        pr["path_p"]["stubs"].as_array().unwrap().iter().filter(|x| x["pads"] == serde_json::json!(["R3.2"])).collect();
    assert_eq!(pull_up.len(), 1, "the D+ pull-up is a stub: {pr}");
    assert!(mm(&pr["skew"]) <= 0.1, "skew along the paths before tuning: {pr}");
    let tu = step_json(p, &["route", "tune"]);
    let bumps = tu["output"]["pairs"][0]["bumps"].as_u64().unwrap();
    assert!(bumps <= 2, "{bumps} skew bumps: {tu}");
    let pairs = step_json(p, &["diffpair", "list"]);
    let pr = &pairs["output"]["pairs"][0];
    let skew = mm(&pr["skew"]);
    assert!(skew <= 0.1, "skew {skew} mm: {pairs}");
    // Mostly coupled: the uncoupled rest is the breakouts at the receptacle, the ESD array and
    // the MCU.
    let (coupled, uncoupled) = (mm(&pr["coupled"]), mm(&pr["uncoupled"]));
    let length = mm(&pr["length_p"]);
    assert!(coupled >= 0.7 * length && uncoupled < 10.0, "coupled {coupled} / uncoupled {uncoupled} of {length} mm");

    // A stub limit tighter than the pull-up's branch is a DRC warning naming it.
    step(p, &["diffpair", "set", "USB_D", "--max-stub", "1mm"]);
    let drc = step_json(p, &["drc"]);
    let stubs: Vec<&Value> =
        drc["diagnostics"].as_array().unwrap().iter().filter(|x| x["code"] == "drc.diffpair_stub").collect();
    assert!(stubs.iter().any(|x| x["subjects"].as_array().unwrap().contains(&serde_json::json!("R3.2"))), "{drc}");
    step(p, &["diffpair", "set", "USB_D", "--max-stub", "5mm"]);

    let drc = step_json(p, &["drc"]);
    assert_eq!(drc["output"]["errors"], 0, "{:?}", codes(&drc));
    assert_eq!(drc["output"]["warnings"], 0, "{:?}", codes(&drc));

    step(p, &["render", "schematic", "out/schematic.svg"]);
    step(p, &["render", "board", "out/board.png"]);
    step(p, &["render", "board3d", "out/board3d.png"]);
    for f in ["schematic.svg", "board.png", "board3d.png"] {
        assert!(p.join("out").join(f).is_file(), "{f}");
    }

    // JLCPCB bundle: the stackup to order with is named and locked.
    let fab = step_json(p, &["fab", "export", "jlcpcb"]);
    assert_eq!(codes(&fab), ["fab.no_suppliers"], "only the expected supplier warning");
    assert_eq!(fab["output"]["stackup"], "JLC04161H-7628");
    let dir = p.join("out/fab/jlcpcb");
    for f in ["usbdev.GTL", "usbdev.G2L", "usbdev.G3L", "usbdev.GBL", "usbdev-PTH.drl", "usbdev-NPTH.drl"] {
        assert!(dir.join(f).is_file(), "{f} missing");
    }
    let lock: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("fab-lock.json")).unwrap()).unwrap();
    assert_eq!(lock["process"]["stackup"], "JLC04161H-7628");
    let cpl = std::fs::read_to_string(dir.join("usbdev-CPL-jlcpcb.csv")).unwrap();
    assert_eq!(cpl.lines().count(), 1 + 30, "header + 30 placed components:\n{cpl}");

    step(p, &["export", "step"]);
    assert!(p.join("out/mcad/usbdev.step").is_file());
}
