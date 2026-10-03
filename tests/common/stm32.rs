//! A larger example board, built through the command registry: an STM32F103 (LQFP-48) with an
//! 8 MHz crystal and load capacitors, reset network, BOOT0/BOOT1 pull-downs, analog supply filter,
//! SWD/UART/I2C/GPIO headers and a user button in the main circuit, plus block instances: USB-C
//! input with ESD protection and CC resistors (`usb`), a 3.3 V LDO with its capacitors (`power`),
//! and three LED indicators (`led_pwr`, `led_act`, `led_status`). ERC clean.

use cadlab::command::{Registry, RunOptions, Session, Step};
use serde_json::{Value, json};

fn pins(list: &[(&str, &str, &str)]) -> Value {
    Value::Array(list.iter().map(|(n, name, kind)| json!({"number": n, "name": name, "kind": kind})).collect())
}

fn owned_pins(list: &[(String, String)]) -> Value {
    Value::Array(list.iter().map(|(n, name)| json!({"number": n, "name": name, "kind": "passive"})).collect())
}

const STM32_PINS: &[(&str, &str, &str)] = &[
    ("1", "VBAT", "power_in"),
    ("2", "PC13", "bidirectional"),
    ("3", "PC14", "bidirectional"),
    ("4", "PC15", "bidirectional"),
    ("5", "OSC_IN", "input"),
    ("6", "OSC_OUT", "output"),
    ("7", "NRST", "input"),
    ("8", "VSSA", "power_in"),
    ("9", "VDDA", "power_in"),
    ("10", "PA0", "bidirectional"),
    ("11", "PA1", "bidirectional"),
    ("12", "PA2", "bidirectional"),
    ("13", "PA3", "bidirectional"),
    ("14", "PA4", "bidirectional"),
    ("15", "PA5", "bidirectional"),
    ("16", "PA6", "bidirectional"),
    ("17", "PA7", "bidirectional"),
    ("18", "PB0", "bidirectional"),
    ("19", "PB1", "bidirectional"),
    ("20", "PB2", "bidirectional"),
    ("21", "PB10", "bidirectional"),
    ("22", "PB11", "bidirectional"),
    ("23", "VSS_1", "power_in"),
    ("24", "VDD_1", "power_in"),
    ("25", "PB12", "bidirectional"),
    ("26", "PB13", "bidirectional"),
    ("27", "PB14", "bidirectional"),
    ("28", "PB15", "bidirectional"),
    ("29", "PA8", "bidirectional"),
    ("30", "PA9", "bidirectional"),
    ("31", "PA10", "bidirectional"),
    ("32", "PA11", "bidirectional"),
    ("33", "PA12", "bidirectional"),
    ("34", "PA13", "bidirectional"),
    ("35", "VSS_2", "power_in"),
    ("36", "VDD_2", "power_in"),
    ("37", "PA14", "bidirectional"),
    ("38", "PA15", "bidirectional"),
    ("39", "PB3", "bidirectional"),
    ("40", "PB4", "bidirectional"),
    ("41", "PB5", "bidirectional"),
    ("42", "PB6", "bidirectional"),
    ("43", "PB7", "bidirectional"),
    ("44", "BOOT0", "input"),
    ("45", "PB8", "bidirectional"),
    ("46", "PB9", "bidirectional"),
    ("47", "VSS_3", "power_in"),
    ("48", "VDD_3", "power_in"),
];

const PORT_B: [&str; 10] = ["PB0", "PB1", "PB8", "PB9", "PB10", "PB11", "PB12", "PB13", "PB14", "PB15"];

fn add(part: &str, refdes: &str) -> Value {
    json!({"cmd": "circuit.add", "args": {"part": part, "refdes": refdes}})
}

fn connect(net: &str, pins: &[&str]) -> Value {
    json!({"cmd": "net.connect", "args": {"net": net, "pins": pins}})
}

/// Builds the board in the session's (empty) project.
pub fn build_stm32_board(r: &Registry, s: &mut Session) {
    let gpio_a: Vec<(String, String)> = (1..=8)
        .map(|i| (i.to_string(), format!("PA{i}")))
        .chain([("9".to_string(), "3V3".to_string()), ("10".to_string(), "GND".to_string())])
        .collect();
    let gpio_b: Vec<(String, String)> =
        PORT_B.iter().enumerate().map(|(i, n)| ((i + 1).to_string(), n.to_string())).collect();
    let mut steps = vec![
        json!({"cmd": "part.create", "args": {"category": "mcu", "manufacturer": "ST", "mpn": "STM32F103C8T6",
            "package": "LQFP-48", "pins": pins(STM32_PINS)}}),
        json!({"cmd": "part.create", "args": {"category": "ldo", "manufacturer": "AMS", "mpn": "AMS1117-3.3", "package": "SOT-223",
            "pins": pins(&[("1", "GND", "power_in"), ("2", "VOUT", "power_out"), ("3", "VIN", "power_in")]),
            "pin_map": {"2": ["2", "4"]}}}),
        json!({"cmd": "part.create", "args": {"category": "ic", "manufacturer": "ST", "mpn": "USBLC6-2SC6", "package": "SOT-23-6",
            "pins": pins(&[("1", "IO1", "passive"), ("2", "GND", "power_in"), ("3", "IO2", "passive"),
                           ("4", "IO2B", "passive"), ("5", "VBUS", "power_in"), ("6", "IO1B", "passive")])}}),
        json!({"cmd": "part.create", "args": {"id": "USB_C", "category": "connector", "description": "USB-C receptacle (USB 2.0)",
            "package": "PinHeader 1x07",
            "pins": pins(&[("1", "VBUS", "passive"), ("2", "CC1", "passive"), ("3", "DM", "passive"), ("4", "DP", "passive"),
                           ("5", "CC2", "passive"), ("6", "GND", "passive"), ("7", "SHIELD", "passive")])}}),
        json!({"cmd": "part.create", "args": {"id": "XTAL_8M", "category": "crystal", "description": "8 MHz crystal",
            "package": "PinHeader 1x02", "params": {"frequency": "8MHz"},
            "pins": pins(&[("1", "1", "passive"), ("2", "2", "passive")])}}),
        json!({"cmd": "part.create", "args": {"id": "BUTTON", "category": "switch", "description": "tactile push button",
            "package": "PinHeader 1x02", "pins": pins(&[("1", "1", "passive"), ("2", "2", "passive")])}}),
        json!({"cmd": "part.create", "args": {"id": "SWD_HDR", "category": "connector", "description": "SWD 1x5 header",
            "package": "PinHeader 1x05",
            "pins": pins(&[("1", "VCC", "passive"), ("2", "SWDIO", "passive"), ("3", "SWCLK", "passive"),
                           ("4", "GND", "passive"), ("5", "NRST", "passive")])}}),
        json!({"cmd": "part.create", "args": {"id": "UART_HDR", "category": "connector", "description": "UART 1x3 header",
            "package": "PinHeader 1x03", "pins": pins(&[("1", "TX", "passive"), ("2", "RX", "passive"), ("3", "GND", "passive")])}}),
        json!({"cmd": "part.create", "args": {"id": "I2C_HDR", "category": "connector", "description": "I2C 1x4 header",
            "package": "PinHeader 1x04",
            "pins": pins(&[("1", "VCC", "passive"), ("2", "GND", "passive"), ("3", "SCL", "passive"), ("4", "SDA", "passive")])}}),
        json!({"cmd": "part.create", "args": {"id": "GPIO_A", "category": "connector", "description": "port A header",
            "package": "PinHeader 1x10", "pins": owned_pins(&gpio_a)}}),
        json!({"cmd": "part.create", "args": {"id": "GPIO_B", "category": "connector", "description": "port B header",
            "package": "PinHeader 1x10", "pins": owned_pins(&gpio_b)}}),
        // Main circuit.
        add("STM32F103C8T6", "U1"),
        add("XTAL_8M", "Y1"),
        add("C 20pF 50V C0G 0402", "C1"),
        add("C 20pF 50V C0G 0402", "C2"),
        add("C 100nF 16V X7R 0402", "C3"),
        add("C 100nF 16V X7R 0402", "C4"),
        add("C 100nF 16V X7R 0402", "C5"),
        add("C 100nF 16V X7R 0402", "C6"),
        add("C 1uF 16V X5R 0402", "C7"),
        add("C 100nF 16V X7R 0402", "C8"),
        add("C 100nF 16V X7R 0402", "C9"),
        add("FB 600R 0603", "FB1"),
        add("R 10k 1% 0402", "R1"),
        add("R 10k 1% 0402", "R2"),
        add("R 10k 1% 0402", "R3"),
        add("R 10k 1% 0402", "R4"),
        add("R 4.7k 1% 0402", "R5"),
        add("R 4.7k 1% 0402", "R6"),
        add("R 1.5k 1% 0402", "R7"),
        add("BUTTON", "SW1"),
        add("BUTTON", "SW2"),
        add("SWD_HDR", "J2"),
        add("UART_HDR", "J3"),
        add("I2C_HDR", "J4"),
        add("GPIO_A", "J5"),
        add("GPIO_B", "J6"),
        connect(
            "3V3",
            &[
                "U1.VBAT", "U1.VDD_1", "U1.VDD_2", "U1.VDD_3", "C3.1", "C4.1", "C5.1", "C6.1", "FB1.1", "R1.1", "R3.1",
                "R5.1", "R6.1", "R7.1", "J2.VCC", "J4.VCC", "J5.3V3",
            ],
        ),
        connect(
            "GND",
            &[
                "U1.VSS_1", "U1.VSS_2", "U1.VSS_3", "U1.VSSA", "C1.2", "C2.2", "C3.2", "C4.2", "C5.2", "C6.2", "C7.2",
                "C8.2", "C9.2", "R2.2", "R4.2", "SW1.2", "SW2.2", "J2.GND", "J3.GND", "J4.GND", "J5.GND",
            ],
        ),
        connect("VDDA", &["U1.VDDA", "FB1.2", "C7.1", "C8.1"]),
        connect("OSC_IN", &["U1.OSC_IN", "Y1.1", "C1.1"]),
        connect("OSC_OUT", &["U1.OSC_OUT", "Y1.2", "C2.1"]),
        connect("NRST", &["U1.NRST", "R1.2", "C9.1", "SW1.1", "J2.NRST"]),
        connect("BOOT0", &["U1.BOOT0", "R2.1"]),
        connect("BUTTON", &["U1.PA0", "R3.2", "SW2.1"]),
        connect("BOOT1", &["U1.PB2", "R4.1"]),
        connect("SCL", &["U1.PB6", "R5.2", "J4.SCL"]),
        connect("SDA", &["U1.PB7", "R6.2", "J4.SDA"]),
        connect("USB_DP", &["U1.PA12", "R7.2"]),
        connect("USB_DM", &["U1.PA11"]),
        connect("SWDIO", &["U1.PA13", "J2.SWDIO"]),
        connect("SWCLK", &["U1.PA14", "J2.SWCLK"]),
        connect("UART_TX", &["U1.PA9", "J3.RX"]),
        connect("UART_RX", &["U1.PA10", "J3.TX"]),
        connect("LED_ACT", &["U1.PB5"]),
        connect("LED_STATUS", &["U1.PC13"]),
        json!({"cmd": "net.no_connect", "args": {"pins": ["U1.PC14", "U1.PC15", "U1.PA15", "U1.PB3", "U1.PB4"]}}),
    ];
    for i in 1..=8 {
        steps.push(connect(&format!("PA{i}"), &[&format!("U1.PA{i}"), &format!("J5.PA{i}")]));
    }
    for n in PORT_B {
        steps.push(connect(n, &[&format!("U1.{n}"), &format!("J6.{n}")]));
    }
    // Blocks: build a prototype, capture it, remove it, then instantiate.
    steps.extend([
        add("USB_C", "J90"),
        add("USBLC6-2SC6", "U90"),
        add("R 5.1k 1% 0402", "R90"),
        add("R 5.1k 1% 0402", "R91"),
        connect("VBUS", &["J90.VBUS", "U90.VBUS"]),
        connect("GND", &["J90.GND", "J90.SHIELD", "U90.GND", "R90.2", "R91.2"]),
        connect("CC1", &["J90.CC1", "R90.1"]),
        connect("CC2", &["J90.CC2", "R91.1"]),
        connect("USB_DP", &["J90.DP", "U90.IO1", "U90.IO1B"]),
        connect("USB_DM", &["J90.DM", "U90.IO2", "U90.IO2B"]),
        json!({"cmd": "block.create", "args": {"name": "usb_in", "components": ["J90", "U90", "R90", "R91"],
            "ports": ["VBUS", "GND", "USB_DP", "USB_DM"], "description": "USB-C input with ESD protection"}}),
        json!({"cmd": "circuit.remove", "args": {"refdes": ["J90", "U90", "R90", "R91"]}}),
        add("AMS1117-3.3", "U91"),
        add("C 10uF 16V X5R 0805", "C90"),
        add("C 10uF 16V X5R 0805", "C91"),
        connect("VBUS", &["U91.VIN", "C90.1"]),
        connect("3V3", &["U91.VOUT", "C91.1"]),
        connect("GND", &["U91.GND", "C90.2", "C91.2"]),
        json!({"cmd": "block.create", "args": {"name": "ldo_3v3", "components": ["U91", "C90", "C91"],
            "ports": ["VBUS", "3V3", "GND"], "description": "3.3 V LDO"}}),
        json!({"cmd": "circuit.remove", "args": {"refdes": ["U91", "C90", "C91"]}}),
        add("R 1k 1% 0402", "R92"),
        add("LED green 0603", "D92"),
        connect("LED_IN", &["R92.1"]),
        connect("LED_A", &["R92.2", "D92.A"]),
        connect("GND", &["D92.K"]),
        json!({"cmd": "block.create", "args": {"name": "led_ind", "components": ["R92", "D92"], "ports": ["LED_IN", "GND"]}}),
        json!({"cmd": "circuit.remove", "args": {"refdes": ["R92", "D92"]}}),
        json!({"cmd": "block.instantiate", "args": {"block": "usb_in", "instance": "usb"}}),
        json!({"cmd": "block.instantiate", "args": {"block": "ldo_3v3", "instance": "power"}}),
        json!({"cmd": "block.instantiate", "args": {"block": "led_ind", "instance": "led_pwr", "connect": {"LED_IN": "3V3"}}}),
        json!({"cmd": "block.instantiate", "args": {"block": "led_ind", "instance": "led_act", "connect": {"LED_IN": "LED_ACT"}}}),
        json!({"cmd": "block.instantiate", "args": {"block": "led_ind", "instance": "led_status",
            "connect": {"LED_IN": "LED_STATUS"}}}),
        json!({"cmd": "net.set", "args": {"nets": ["VBUS", "VDDA"], "driven": true}}),
    ]);
    let steps: Vec<Step> = serde_json::from_value(Value::Array(steps)).unwrap();
    r.execute_batch(s, steps, RunOptions::default()).unwrap_or_else(|f| panic!("step {:?}: {}", f.step, f.error));
}
