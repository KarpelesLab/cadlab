//! Agent session 5 (docs/TESTING.md, "Agent sessions"): a mixed-signal board with hierarchical
//! blocks, designed through the MCP server (`cadlab mcp`, JSON-RPC over stdio) and replayed as a
//! regression test.
//!
//! Prompt: "A 2-channel 24-bit ADC data logger on four layers: STM32-class MCU, precision ADC
//! with a voltage reference, an analog front end (one op-amp block instantiated twice), separate
//! AGND and DGND joined at a star point with split pours, LDOs for the analog and digital rails,
//! USB-C and a microSD card. Parts, circuit, hierarchy, ERC, placement, routing, pours, DRC,
//! SPICE where it applies, fab outputs."
//!
//! Every call is a `tools/call` as the agent made it, after the session's friction was fixed
//! (DECISIONS D66): the AFE channel is captured as the block's first instance
//! (`block.create --instance`), the star point is a net-tie part that ERC follows (AGND is fed
//! through it), ERC output narrows like DRC's, `place.hint` takes block instances and regions
//! (analog section left, digital right) and edge hints win over earlier groups' rooms, DRC
//! reports signals crossing the ground split (waived for the ADC's SPI lines, with a reason),
//! SPICE exports one block instance on its own and joins the split grounds through the net
//! tie, the net tie is left out of the BOM, the placement file and the STEP model, and moving a
//! routed part names the routing it leaves behind.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

mod common;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent_session5")
}

fn fixture(name: &str) -> Value {
    let p = fixtures().join(name);
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    /// Starts `cadlab mcp` isolated from the user's configuration (no suppliers).
    fn start(cwd: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cadlab"))
            .arg("mcp")
            .current_dir(cwd)
            .env("XDG_CONFIG_HOME", cwd.join(".no-config"))
            .env("XDG_CACHE_HOME", cwd.join(".no-cache"))
            .env("XDG_DATA_HOME", cwd.join(".no-data"))
            .env_remove("CADLAB_CATALOGS")
            .env_remove("DIGIKEY_CLIENT_ID")
            .env_remove("DIGIKEY_CLIENT_SECRET")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut c = Client { child, stdin, stdout, next_id: 1 };
        let init = c.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "session5", "version": "0"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "cadlab");
        c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        c
    }

    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "server closed stdout");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}));
        assert!(r.get("error").is_none(), "protocol error for {tool} {args}: {r}");
        r["result"].clone()
    }

    /// `tool` with `action` and `args`, which must succeed: the structured content.
    fn ok(&mut self, tool: &str, action: &str, args: Value) -> Value {
        let r = self.call(tool, json!({"action": action, "args": args}));
        assert_eq!(r["isError"], false, "{tool}.{action} {args} failed: {}", text(&r));
        r["structuredContent"].clone()
    }

    /// `tool` with `action` and `args`, which must fail: the structured error.
    fn err(&mut self, tool: &str, action: &str, args: Value) -> Value {
        let r = self.call(tool, json!({"action": action, "args": args}));
        assert_eq!(r["isError"], true, "{tool}.{action} {args} should fail: {}", text(&r));
        r["structuredContent"]["error"].clone()
    }

    /// Several commands as one transaction.
    fn batch(&mut self, steps: Vec<Value>) -> Value {
        let r = self.call("batch", json!({"steps": steps}));
        assert_eq!(r["isError"], false, "batch failed: {}", text(&r));
        r["structuredContent"].clone()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(r: &Value) -> String {
    r["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n")
}

fn diags<'a>(out: &'a Value, code: &str) -> Vec<&'a Value> {
    out["diagnostics"].as_array().into_iter().flatten().filter(|d| d["code"] == code).collect()
}

fn codes(out: &Value) -> Vec<String> {
    out["diagnostics"].as_array().into_iter().flatten().filter_map(|d| d["code"].as_str().map(String::from)).collect()
}

fn pins(list: &[(&str, &str, &str)]) -> Value {
    list.iter().map(|(n, name, kind)| json!({"number": n, "name": name, "kind": kind})).collect()
}

fn step(cmd: &str, args: Value) -> Value {
    json!({"cmd": cmd, "args": args})
}

fn mm(v: &Value) -> f64 {
    v.as_str().unwrap().trim_end_matches("mm").parse::<f64>().unwrap()
}

/// The parts: ICs from pin lists, connectors from pad lists, the net-tie star point, generic
/// passives. One batch, as the agent sent it.
fn parts(c: &mut Client) {
    let ldo = pins(&[
        ("1", "VIN", "power_in"),
        ("2", "GND", "power_in"),
        ("3", "EN", "input"),
        ("4", "NC", "no_connect"),
        ("5", "VOUT", "power_out"),
    ]);
    let session3 = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent_session3/usb_c_16p_pads.json");
    let usbc_pads: Value = serde_json::from_str(&std::fs::read_to_string(session3).unwrap()).unwrap();
    let mut steps = vec![
        step(
            "part.create",
            json!({"category": "mcu", "mpn": "STM32F411CEU6", "manufacturer": "ST", "description": "Cortex-M4F 100MHz, USB OTG FS",
                "package": "QFN-48 7x7mm P0.5mm EP5.6mm", "pins": fixture("stm32f411_pins.json")}),
        ),
        step(
            "part.create",
            json!({"category": "ic", "mpn": "ADS1220IPWR", "manufacturer": "TI", "description": "24-bit delta-sigma ADC, PGA, SPI",
                "package": "TSSOP-16", "pins": fixture("ads1220_pins.json")}),
        ),
        step(
            "part.create",
            json!({"category": "ic", "mpn": "REF5025IDR", "manufacturer": "TI", "description": "2.5V precision voltage reference",
                "package": "SOIC-8", "pins": pins(&[("1", "DNC1", "no_connect"), ("2", "VIN", "power_in"), ("3", "TEMP", "output"),
                    ("4", "GND", "power_in"), ("5", "TRIM", "passive"), ("6", "VOUT", "power_out"), ("7", "NC", "no_connect"), ("8", "DNC2", "no_connect")])}),
        ),
        step(
            "part.create",
            json!({"category": "ic", "mpn": "OPA376AIDBVR", "manufacturer": "TI", "description": "Precision low-noise RRIO op-amp",
                "package": "SOT-23-5", "pins": pins(&[("1", "OUT", "output"), ("2", "V-", "power_in"), ("3", "+IN", "input"), ("4", "-IN", "input"), ("5", "V+", "power_in")])}),
        ),
        step(
            "part.create",
            json!({"category": "ldo", "mpn": "ADP150AUJZ-3.3", "manufacturer": "Analog Devices", "description": "Low-noise LDO 3.3V (analog)", "package": "SOT-23-5", "pins": ldo}),
        ),
        step(
            "part.create",
            json!({"category": "ldo", "mpn": "AP2112K-3.3TRG1", "manufacturer": "Diodes", "description": "600mA LDO 3.3V (digital)", "package": "SOT-23-5", "pins": ldo}),
        ),
        step(
            "part.create",
            json!({"category": "ic", "mpn": "USBLC6-2SC6", "manufacturer": "ST", "description": "USB ESD protection", "package": "SOT-23-6",
                "pins": pins(&[("1", "IO1", "passive"), ("2", "GND", "power_in"), ("3", "IO2", "passive"), ("4", "IO2b", "passive"), ("5", "VBUS", "power_in"), ("6", "IO1b", "passive")])}),
        ),
        step(
            "footprint.create",
            json!({"name": "USB_C_16P", "pads": usbc_pads, "body": {"width": "8.94mm", "length": "7.3mm", "height": "3.2mm", "at": ["0mm", "-3.4mm"]}}),
        ),
        step(
            "part.create",
            json!({"category": "connector", "mpn": "TYPE-C-31-M-12", "manufacturer": "HRO", "description": "USB-C 2.0 receptacle",
                "footprint": "USB_C_16P", "pins": fixture("usb_c_16p_pins.json")}),
        ),
        step(
            "footprint.create",
            json!({"name": "MICROSD_PUSH", "pads": fixture("microsd_pads.json"), "body": {"width": "14mm", "length": "11mm", "height": "1.8mm", "at": ["0mm", "5mm"]}}),
        ),
        step(
            "part.create",
            json!({"category": "connector", "mpn": "DM3AT-SF-PEJM5", "manufacturer": "Hirose", "description": "microSD socket",
                "footprint": "MICROSD_PUSH", "pins": fixture("microsd_pins.json")}),
        ),
        step(
            "part.create",
            json!({"category": "crystal", "id": "XTAL_8MHz_3225", "description": "8 MHz crystal", "params": {"frequency": "8MHz"},
                "package": "DFN-4 3.2x2.5mm P1.6mm", "pins": pins(&[("1", "X1", "passive"), ("2", "GND", "passive"), ("3", "X2", "passive"), ("4", "GND", "passive")])}),
        ),
        step(
            "part.create",
            json!({"category": "connector", "id": "SWD_2x05_1.27", "description": "Cortex debug header", "package": "PinHeader 2x05 P1.27mm",
                "pins": pins(&[("1", "VTREF", "passive"), ("2", "SWDIO", "passive"), ("3", "GND", "passive"), ("4", "SWCLK", "passive"), ("5", "GND", "passive"),
                    ("6", "SWO", "passive"), ("7", "KEY", "no_connect"), ("8", "NC", "no_connect"), ("9", "GNDD", "passive"), ("10", "NRST", "passive")])}),
        ),
        step(
            "part.create",
            json!({"category": "connector", "id": "AIN_TERM_4P", "description": "Analog input terminal", "package": "PinHeader 1x04 P3.5mm",
                "pins": pins(&[("1", "IN1", "passive"), ("2", "AGND1", "passive"), ("3", "IN2", "passive"), ("4", "AGND2", "passive")])}),
        ),
        // The star point: two touching pads whose nets the footprint joins on purpose.
        step(
            "footprint.create",
            json!({"name": "NetTie_2P_0.5mm", "pads": [
                {"number": "1", "at": ["-0.4mm", "0mm"], "size": ["0.5mm", "0.5mm"], "shape": "rect"},
                {"number": "2", "at": ["0.4mm", "0mm"], "size": ["0.5mm", "0.5mm"], "shape": "rect"}]}),
        ),
        step("footprint.set", json!({"name": "NetTie_2P_0.5mm", "net_ties": [["1", "2"]]})),
        step(
            "part.create",
            json!({"category": "other", "id": "NETTIE_AGND_GND", "description": "Star point AGND/GND", "footprint": "NetTie_2P_0.5mm",
                "pins": pins(&[("1", "A", "passive"), ("2", "B", "passive")])}),
        ),
    ];
    for spec in [
        "R 10k 1% 0402",
        "R 1k 0.1% 0603",
        "R 100 1% 0603",
        "R 5.1k 1% 0402",
        "R 1k 1% 0402",
        "C 100nF 16V X7R 0402",
        "C 10nF 50V C0G 0603",
        "C 1uF 16V X7R 0603",
        "C 10uF 16V X5R 0805",
        "C 4.7uF 16V X5R 0603",
        "C 12pF 50V C0G 0402",
        "C 2.2uF 16V X7R 0603",
        "LED green 0603",
        "FB 600R 0603",
    ] {
        steps.push(step("part.generic", json!({"spec": spec})));
    }
    c.batch(steps);
}

/// One analog front-end channel built once and captured as the block's first instance (CH1),
/// then a second instance (CH2).
fn afe_blocks(c: &mut Client) {
    let afe = [
        ("OPA376AIDBVR", "U1"),
        ("R_1k_1pct_0402", "R1"),
        ("C_10nF_50V_C0G_0603", "C1"),
        ("R_1k_0.1pct_0603", "R2"),
        ("R_1k_0.1pct_0603", "R3"),
        ("R_100_1pct_0603", "R4"),
        ("C_10nF_50V_C0G_0603", "C2"),
        ("C_100nF_16V_X7R_0402", "C3"),
    ];
    let mut steps: Vec<Value> = afe.iter().map(|(p, r)| step("circuit.add", json!({"part": p, "refdes": r}))).collect();
    for (net, pins) in [
        ("AIN1_IN", &["R1.1"][..]),
        ("FILT", &["R1.2", "C1.1", "U1.+IN"]),
        ("FB", &["U1.-IN", "R2.2", "R3.1"]),
        ("OA_OUT", &["U1.OUT", "R2.1", "R4.1"]),
        ("ADC_AIN0", &["R4.2", "C2.1"]),
        ("+3V3A", &["U1.V+", "C3.1"]),
        ("AGND", &["U1.V-", "C1.2", "R3.2", "C2.2", "C3.2"]),
    ] {
        steps.push(step("net.connect", json!({"net": net, "pins": pins})));
    }
    c.batch(steps);
    let refs: Vec<&str> = afe.iter().map(|x| x.1).collect();
    // Without `instance` (previewed): the components would stay outside any instance, and the
    // result says how to make them the first one.
    let r = c.call("block", json!({"action": "create", "args": {"name": "afe", "components": refs}, "dry_run": true}));
    let preview = r["structuredContent"].clone();
    assert_eq!(diags(&preview, "block.not_instance").len(), 1, "{preview}");
    let b = c.ok(
        "block",
        "create",
        json!({"name": "afe", "components": refs, "instance": "CH1", "ports": ["AIN1_IN", "ADC_AIN0", "+3V3A", "AGND"],
            "description": "Gain-2 buffer with input and output RC filters"}),
    );
    assert_eq!(b["output"]["instances"], json!(["CH1"]), "{b}");
    // The internal nets are named like a later instance's.
    let n = c.ok("net", "list", json!({}));
    let names: Vec<&str> = n["output"]["nets"].as_array().unwrap().iter().filter_map(|x| x["name"].as_str()).collect();
    assert!(names.contains(&"CH1/FB") && !names.contains(&"FB"), "{names:?}");
    // A component already in an instance cannot be captured into another one.
    let e = c.err("block", "create", json!({"name": "afe2", "components": ["U1"], "instance": "CH9"}));
    assert_eq!(e["code"], "block.component_in_instance", "{e}");
    let i = c.ok(
        "block",
        "instantiate",
        json!({"block": "afe", "instance": "CH2", "connect": {"AIN1_IN": "AIN2_IN", "ADC_AIN0": "ADC_AIN1"}}),
    );
    assert_eq!(i["output"]["components"]["U1"], "U2", "{i}");
    let l = c.ok("circuit", "list", json!({"instance": "CH2"}));
    let comps = l["output"]["components"].as_array().unwrap();
    assert_eq!(comps.len(), 8, "{l}");
    assert!(comps.iter().all(|x| x["instance"] == "CH2"), "{l}");
    let e = c.err("circuit", "list", json!({"instance": "CH3"}));
    assert_eq!(e["code"], "block.instance_not_found", "{e}");
}

/// The rest of the circuit: ADC, reference, MCU, regulators, connectors, net tie, passives,
/// connections.
fn circuit(c: &mut Client) {
    let mut steps = Vec::new();
    for (part, r) in [
        ("ADS1220IPWR", "U3"),
        ("REF5025IDR", "U4"),
        ("STM32F411CEU6", "U5"),
        ("AP2112K-3.3TRG1", "U6"),
        ("ADP150AUJZ-3.3", "U7"),
        ("USBLC6-2SC6", "U8"),
        ("TYPE-C-31-M-12", "J1"),
        ("DM3AT-SF-PEJM5", "J2"),
        ("SWD_2x05_1.27", "J3"),
        ("AIN_TERM_4P", "J4"),
        ("XTAL_8MHz_3225", "Y1"),
        ("FB_600_0603", "FB1"),
        ("LED_green_0603", "D1"),
        ("NETTIE_AGND_GND", "NT1"),
    ] {
        steps.push(step("circuit.add", json!({"part": part, "refdes": r})));
    }
    for (part, n) in [
        ("C_10uF_16V_X5R_0805", 4),
        ("C_1uF_16V_X7R_0603", 5),
        ("C_100nF_16V_X7R_0402", 9),
        ("C_12pF_50V_C0G_0402", 2),
        ("C_2.2uF_16V_X7R_0603", 1),
        ("C_4.7uF_16V_X5R_0603", 1),
        ("R_5.1k_1pct_0402", 2),
        ("R_10k_1pct_0402", 6),
        ("R_1k_1pct_0402", 1),
    ] {
        steps.push(step("circuit.add", json!({"part": part, "count": n})));
    }
    c.batch(steps);

    // Before wiring, ERC lists every open pin: the output narrows like DRC's.
    let e = c.ok("circuit", "erc", json!({"codes": ["erc.unconnected"], "limit": 3}));
    assert_eq!(e["output"]["listed"], 3, "{e}");
    assert!(e["output"]["not_listed"].as_u64().unwrap() > 10, "{e}");
    assert!(e["output"]["by_code"].as_array().unwrap().iter().any(|x| x["code"] == "erc.power_unconnected"), "{e}");
    assert_eq!(codes(&e).len(), 3);

    #[rustfmt::skip]
    let nets: &[(&str, &[&str])] = &[
        ("VBUS", &["J1.VBUS", "U8.VBUS", "U6.VIN", "U6.EN", "C7.1", "FB1.1"]),
        ("GND", &["J1.GND", "J1.SHIELD", "U8.GND", "U6.GND", "C7.2", "C8.2", "C10.2", "R9.2", "R10.2", "U5.VSS", "U5.VSSA",
            "C17.2", "C18.2", "C19.2", "C20.2", "C21.2", "C22.2", "C23.2", "C25.2", "C26.2", "C27.2", "C28.2", "Y1.GND", "R11.2",
            "J3.GND", "J3.GNDD", "J2.VSS", "J2.SHIELD", "U3.DGND", "U3.CLK", "NT1.2"]),
        ("+3V3", &["U6.VOUT", "C8.1", "C10.1", "U5.VDD", "U5.VBAT", "U5.VDDA", "C18.1", "C19.1", "C20.1", "C21.1", "C28.1",
            "U3.DVDD", "C17.1", "J2.VDD", "C22.1", "R12.1", "R13.1", "R14.1", "R15.1", "R16.1", "J3.VTREF", "R17.1"]),
        ("VBUS_A", &["FB1.2", "U7.VIN", "U7.EN", "C11.1", "U4.VIN", "C13.1"]),
        ("+3V3A", &["U7.VOUT", "C12.1", "U3.AVDD", "C15.1", "C16.1"]),
        ("AGND", &["U7.GND", "C11.2", "C12.2", "C13.2", "C14.2", "C15.2", "C16.2", "C9.2", "C24.2", "U4.GND", "U3.AVSS",
            "U3.REFN0", "J4.AGND1", "J4.AGND2"]),
        ("VREF", &["U4.VOUT", "C9.1", "U3.REFP0", "C24.1"]),
        ("REF_NR", &["U4.TRIM", "C14.1"]),
        ("ADC_AIN0", &["U3.AIN0"]), ("ADC_AIN1", &["U3.AIN1"]),
        ("AIN1_IN", &["J4.IN1"]), ("AIN2_IN", &["J4.IN2"]),
        ("ADC_SCLK", &["U3.SCLK", "U5.PA5"]), ("ADC_CS", &["U3.CS", "U5.PA4"]), ("ADC_MISO", &["U3.DOUT", "U5.PA6"]),
        ("ADC_MOSI", &["U3.DIN", "U5.PA7"]), ("ADC_DRDY", &["U3.DRDY", "U5.PB0"]),
        ("USB_DP", &["J1.DP1", "J1.DP2", "U8.IO1", "U8.IO1b", "U5.PA12"]),
        ("USB_DN", &["J1.DN1", "J1.DN2", "U8.IO2", "U8.IO2b", "U5.PA11"]),
        ("CC1", &["J1.CC1", "R9.1"]), ("CC2", &["J1.CC2", "R10.1"]),
        ("VCAP1", &["U5.VCAP1", "C27.1"]), ("NRST", &["U5.NRST", "C23.1", "J3.NRST"]), ("BOOT0", &["U5.BOOT0", "R11.1"]),
        ("HSE_IN", &["U5.PH0", "Y1.X1", "C25.1"]), ("HSE_OUT", &["U5.PH1", "Y1.X2", "C26.1"]),
        ("SWDIO", &["U5.PA13", "J3.SWDIO"]), ("SWCLK", &["U5.PA14", "J3.SWCLK"]), ("SWO", &["U5.PB3", "J3.SWO"]),
        ("SD_CS", &["U5.PB12", "J2.DAT3", "R16.2"]), ("SD_CLK", &["U5.PB13", "J2.CLK"]), ("SD_MISO", &["U5.PB14", "J2.DAT0", "R13.2"]),
        ("SD_MOSI", &["U5.PB15", "J2.CMD", "R12.2"]), ("SD_DAT1", &["J2.DAT1", "R14.2"]), ("SD_DAT2", &["J2.DAT2", "R15.2"]),
        ("LED_K", &["D1.K", "U5.PC13"]), ("LED_A", &["D1.A", "R17.2"]),
    ];
    let mut steps: Vec<Value> = nets.iter().map(|(n, p)| step("net.connect", json!({"net": n, "pins": p}))).collect();
    #[rustfmt::skip]
    let nc = ["U3.AIN2", "U3.AIN3", "U4.TEMP", "U4.DNC1", "U4.DNC2", "U4.NC", "U6.NC", "U7.NC", "J1.SBU1", "J1.SBU2",
        "J3.KEY", "J3.NC", "U5.PC14", "U5.PC15", "U5.PA0", "U5.PA1", "U5.PA2", "U5.PA3", "U5.PB1", "U5.PB2", "U5.PB10",
        "U5.PA8", "U5.PA9", "U5.PA10", "U5.PA15", "U5.PB4", "U5.PB5", "U5.PB6", "U5.PB7", "U5.PB8", "U5.PB9"];
    steps.push(step("net.no_connect", json!({"pins": nc})));
    steps.push(step("net.set", json!({"nets": ["VBUS"], "driven": true})));
    c.batch(steps);

    // AGND alone has no supply: the hint points at a net tie rather than "mark it driven".
    let e = c.ok("circuit", "erc", json!({"severity": "error"}));
    let undriven = diags(&e, "erc.power_not_driven");
    assert_eq!(undriven.len(), 1, "{e}");
    assert!(undriven[0]["hint"].as_str().unwrap().contains("net-tie"), "{e}");
    // The star point joins AGND to GND: ERC follows the net tie.
    c.ok("net", "connect", json!({"net": "AGND", "pins": ["NT1.1"]}));
    let e = c.ok("circuit", "erc", json!({"lint": true}));
    assert_eq!((e["output"]["errors"].clone(), e["output"]["warnings"].clone()), (json!(0), json!(0)), "{e}");

    // The summary names the block instances.
    let s = c.call("circuit", json!({"action": "summary"}));
    let t = text(&s);
    assert!(t.contains("block instances:") && t.contains("CH2 (afe;") && t.contains("U2: OPA376AIDBVR"), "{t}");
}

/// Board, rules, placement with edge and region hints.
fn board_and_placement(c: &mut Client, project: &Path) {
    c.batch(vec![
        step("board.setup", json!({"layers": 4})),
        step("board.fab_stackup", json!({"fab": "jlcpcb", "stackup": "JLC04161H-7628"})),
        step("board.outline", json!({"width": "70mm", "height": "50mm", "corner_radius": "2mm"})),
        step("board.rules", json!({"fab": "jlcpcb"})),
        step("board.hole", json!({"at": ["3.5mm", "3.5mm"], "drill": "3.2mm", "name": "MH1"})),
        step("board.hole", json!({"at": ["66.5mm", "3.5mm"], "drill": "3.2mm", "name": "MH2"})),
        step("board.hole", json!({"at": ["3.5mm", "46.5mm"], "drill": "3.2mm", "name": "MH3"})),
        step("board.hole", json!({"at": ["66.5mm", "46.5mm"], "drill": "3.2mm", "name": "MH4"})),
        step("impedance.solve", json!({"target": "90ohm", "gap": "0.15mm", "netclass": "USB"})),
        step("diffpair.add", json!({"p": "USB_DP", "n": "USB_DN", "name": "USB", "class": "USB", "max_skew": "0.1mm"})),
    ]);
    for (r, e) in [("J4", "left"), ("J1", "right"), ("J2", "top"), ("J3", "bottom")] {
        c.ok("place", "hint", json!({"refdes": [r], "edge": e}));
    }
    // Analog section left of x = 30 mm, digital right: block instances stand for their parts.
    let analog = json!([
        "CH1", "CH2", "U3", "U4", "U7", "J4", "FB1", "C9", "C11", "C12", "C13", "C14", "C15", "C16", "C24", "NT1"
    ]);
    let h =
        c.ok("place", "hint", json!({"refdes": analog, "region": {"from": ["0mm", "0mm"], "to": ["30mm", "50mm"]}}));
    assert!(h["output"]["hints"]["U2"]["region"].is_object(), "CH2's op-amp is hinted: {h}");
    let digital = json!(["U5", "U6", "U8", "J1", "J2", "J3", "Y1", "D1"]);
    c.ok("place", "hint", json!({"refdes": digital, "region": {"from": ["30mm", "0mm"], "to": ["70mm", "50mm"]}}));
    let e =
        c.err("place", "hint", json!({"refdes": ["D1"], "region": {"from": ["80mm", "0mm"], "to": ["90mm", "5mm"]}}));
    assert_eq!(e["code"], "place.region_outside", "{e}");
    let pl = c.ok("place", "auto", json!({}));
    assert!(pl["output"]["unplaced"].as_array().is_none_or(Vec::is_empty), "{pl}");

    // Every hinted part's courtyard is inside its region; J4 at the left edge (its edge hint
    // used to lose to the analog groups' rooms, which covered that edge).
    let p = cadlab::model::Project::load(project).unwrap();
    let hints = p.board().placement_hints.clone();
    for (r, h) in &hints {
        let Some(g) = h.region else { continue };
        let b = cadlab::board::place::courtyard_box(&p, r).unwrap();
        let (x0, x1) = (g.from.x.0.min(g.to.x.0), g.from.x.0.max(g.to.x.0));
        assert!(b.x0 >= x0 && b.x1 <= x1, "{r} at x {}..{} outside {}..{}", b.x0, b.x1, x0, x1);
    }
    let j4 = cadlab::board::place::courtyard_box(&p, "J4").unwrap();
    assert!(j4.x0 < 2_000_000, "J4 at the left edge: x0 {}", j4.x0);
    let at = |r: &str| mm(&pl["output"]["placements"][r]["at"][0]);
    assert!(at("U1") < 30.0 && at("U2") < 30.0 && at("U5") > 30.0, "{pl}");
}

/// Split pours, routing, DRC with the split-crossing check and waivers.
fn route_and_check(c: &mut Client) {
    for (name, net, layer, x0, x1) in [
        ("AGND_IN1", "AGND", "In1.Cu", "0mm", "30mm"),
        ("GND_IN1", "GND", "In1.Cu", "30mm", "70mm"),
        ("P3V3A_IN2", "+3V3A", "In2.Cu", "0mm", "30mm"),
        ("P3V3_IN2", "+3V3", "In2.Cu", "30mm", "70mm"),
        ("AGND_B", "AGND", "B.Cu", "0mm", "30mm"),
        ("GND_B", "GND", "B.Cu", "30mm", "70mm"),
    ] {
        c.ok(
            "zone",
            "add",
            json!({"name": name, "net": net, "layers": [layer], "outline": {"rect": {"from": [x0, "0mm"], "to": [x1, "50mm"]}}}),
        );
    }
    c.ok("route", "diffpair", json!({"pairs": ["USB"]}));
    let r = c.ok("route", "all", json!({"budget": "120s"}));
    assert_eq!(r["output"]["stats"]["failed"], 0, "{r}");
    c.ok("zone", "fill", json!({}));
    let s = c.ok("route", "status", json!({}));
    assert_eq!(s["output"]["unrouted"], 0, "{s}");

    // No errors; the ADC's SPI lines cross the ground split (the ADC straddles it).
    let d = c.call("drc", json!({}));
    let d = d["structuredContent"].clone();
    assert_eq!(d["output"]["errors"], 0, "{d}");
    let crossing: Vec<String> = diags(&d, "drc.split_crossing")
        .iter()
        .map(|x| x["subjects"][0].as_str().unwrap().to_string())
        .filter(|s| s.starts_with("net:ADC_"))
        .collect();
    assert!(crossing.len() >= 3, "{d}");
    for net in &crossing {
        c.ok(
            "drc",
            "waive",
            json!({"code": "drc.split_crossing", "subjects": [net],
                "reason": "ADC SPI lines cross the AGND/GND split at the ADC, next to the NT1 star point"}),
        );
    }
    let d = c.call("drc", json!({}));
    let d = d["structuredContent"].clone();
    assert_eq!(d["output"]["errors"], 0, "{d}");
    assert_eq!(d["output"]["waived"].as_u64().unwrap(), crossing.len() as u64, "{d}");
    assert!(
        diags(&d, "drc.split_crossing").iter().all(|x| !x["subjects"][0].as_str().unwrap().starts_with("net:ADC_"))
    );
}

/// SPICE: one channel alone (ideal op-amp model), and the whole circuit with the net tie.
fn spice(c: &mut Client, project: &Path) {
    let lib = fixtures().join("opamp_ideal.lib");
    c.ok(
        "part",
        "set",
        json!({"id": "OPA376AIDBVR", "params": {"spice_model": "OPAMP_IDEAL", "spice_lib": lib.to_str().unwrap(), "spice_pins": "OUT V- +IN -IN V+"}}),
    );
    c.ok("net", "set", json!({"nets": ["+3V3A"], "voltage": "3.3V"}));
    c.ok("net", "set", json!({"nets": ["AIN1_IN"], "voltage": "1V"}));
    let r = c.ok(
        "export",
        "spice",
        json!({"path": "out/spice/ch1.cir", "components": ["CH1"], "ground": "AGND", "supplies": true,
            "control": ["op", "print v(ADC_AIN0)"]}),
    );
    assert!(r["output"]["placeholders"].as_array().is_none_or(Vec::is_empty), "{r}");
    let cir = std::fs::read_to_string(project.join("out/spice/ch1.cir")).unwrap();
    assert!(cir.contains("XU1 CH1_OA_OUT 0 CH1_FILT CH1_FB _P3V3A OPAMP_IDEAL"), "{cir}");
    assert!(!cir.contains("U2") && !cir.contains("U5"), "only CH1: {cir}");
    // The whole circuit: AGND reaches node 0 through the net tie.
    c.ok("export", "spice", json!({"path": "out/spice/all.cir"}));
    let all = std::fs::read_to_string(project.join("out/spice/all.cir")).unwrap();
    assert!(all.lines().any(|l| l == "RNT1 AGND 0 1m"), "{all}");
    // ngspice: gain 2 on CH1.
    if let Some(ngspice) = common::oracle::optional(common::oracle::Oracle::Ngspice) {
        let path = project.join("out/spice/ch1.cir");
        let out = common::oracle::run(&ngspice, &["-b", path.to_str().unwrap()]);
        let v: f64 = out
            .lines()
            .find_map(|l| l.trim().strip_prefix("v(adc_ain0) = "))
            .unwrap_or_else(|| panic!("no v(adc_ain0) in ngspice output:\n{out}"))
            .trim()
            .parse()
            .unwrap();
        assert!((v - 2.0).abs() < 1e-3, "v(adc_ain0) = {v}");
    }
    c.ok("net", "set", json!({"nets": ["AIN1_IN"], "voltage": ""}));
}

/// Fab outputs: the net tie is copper only (no BOM line, no placement, no STEP body).
fn outputs(c: &mut Client, project: &Path) {
    let r = c.ok("fab", "export", json!({"fab": "jlcpcb", "dir": "out/jlcpcb"}));
    assert!(r["output"]["files"].as_array().unwrap().len() >= 10, "{r}");
    for f in std::fs::read_dir(project.join("out/jlcpcb")).unwrap().flatten() {
        let name = f.file_name().to_string_lossy().to_string();
        if name.ends_with(".csv") {
            let csv = std::fs::read_to_string(f.path()).unwrap();
            assert!(!csv.contains("NT1"), "{name}: {csv}");
            assert!(csv.contains("U3"), "{name}");
        }
    }
    let b = c.ok("bom", "list", json!({}));
    assert!(!b.to_string().contains("NETTIE"), "{b}");
    let s = c.ok("export", "step", json!({}));
    assert!(diags(&s, "export.no_body").is_empty(), "{s}");
    c.ok("render", "schematic", json!({"path": "out/schematic.svg"}));
    c.ok("render", "board", json!({"path": "out/board.png", "px_per_mm": 8}));
    let k = c.ok("board", "export_kicad", json!({"path": "out/kicad/logger.kicad_pcb"}));
    assert_eq!(k["output"]["footprints"], 61, "{k}");
    let pcb = std::fs::read_to_string(project.join("out/kicad/logger.kicad_pcb")).unwrap();
    assert!(pcb.contains("(attr smd exclude_from_pos_files exclude_from_bom)"), "net tie excluded in KiCad too");
}

/// Moving a routed part names the routing it leaves behind; `rip: true` removes it.
fn move_routed(c: &mut Client) {
    let r = c.ok("place", "rotate", json!({"refdes": ["Y1"], "by": 180}));
    let w = diags(&r, "place.moved_routed");
    assert_eq!(w.len(), 1, "{r}");
    assert!(w[0]["message"].as_str().unwrap().contains("HSE_IN"), "{r}");
    c.ok("history", "undo", json!({}));
    let r = c.ok("place", "rotate", json!({"refdes": ["Y1"], "by": 180, "rip": true}));
    assert_eq!(diags(&r, "place.ripped").len(), 1, "{r}");
    let s = c.ok("route", "status", json!({}));
    assert!(s["output"]["unrouted"].as_u64().unwrap() > 0, "{s}");
    let r = c.ok("route", "nets", json!({"nets": ["HSE_IN", "HSE_OUT", "GND"]}));
    assert_eq!(r["output"]["stats"]["failed"], 0, "{r}");
}

#[test]
fn mixed_signal_logger_with_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Client::start(dir.path());
    c.ok("project", "new", json!({"path": "logger", "name": "adc-logger", "targets": ["jlcpcb"]}));
    let project = dir.path().join("logger");
    parts(&mut c);
    afe_blocks(&mut c);
    circuit(&mut c);
    board_and_placement(&mut c, &project);
    route_and_check(&mut c);
    spice(&mut c, &project);
    outputs(&mut c, &project);
    move_routed(&mut c);
}
