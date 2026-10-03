//! `cadlab config`: user settings (supplier credentials). CLI-only on purpose: secrets are typed
//! by the user, never passed through an agent or MCP (DECISIONS D17).

use std::io::{BufRead, IsTerminal, Write};

use cadlab::config::{self, DigiKeySettings, UserConfig, mask};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::json;

/// The `config` subcommand tree.
pub fn command() -> Command {
    Command::new("config")
        .about("User settings: supplier credentials (stored in your config directory, never in projects)")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(Command::new("path").about("Print the settings file location"))
        .subcommand(Command::new("show").about("Show settings (secrets masked)"))
        .subcommand(
            Command::new("digikey")
                .about("Set DigiKey API credentials (prompts for what is not given; verifies them before saving)")
                .long_about(
                    "Set DigiKey API credentials. Prompts for the client ID and, without echo, the client secret.\n\
                     Press Enter to keep a stored value. Without a terminal, the secret is read from the first line\n\
                     of standard input, e.g.: printf '%s\\n' \"$SECRET\" | cadlab config digikey --client-id ID",
                )
                .arg(
                    Arg::new("client_id")
                        .long("client-id")
                        .value_name("ID")
                        .help("OAuth client ID of your DigiKey app"),
                )
                .arg(
                    Arg::new("site")
                        .long("site")
                        .value_name("SITE")
                        .help("Locale site: US, DE, UK, ... (default US)"),
                )
                .arg(
                    Arg::new("currency")
                        .long("currency")
                        .value_name("CUR")
                        .help("Price currency: USD, EUR, ... (default USD)"),
                )
                .arg(
                    Arg::new("language")
                        .long("language")
                        .value_name("LANG")
                        .help("Language (default en)"),
                )
                .arg(
                    Arg::new("sandbox")
                        .long("sandbox")
                        .action(ArgAction::SetTrue)
                        .help("Use DigiKey's sandbox API"),
                )
                .arg(
                    Arg::new("no_verify")
                        .long("no-verify")
                        .action(ArgAction::SetTrue)
                        .help("Save without checking the credentials against DigiKey"),
                ),
        )
        .subcommand(
            Command::new("remove").about("Remove stored credentials").arg(
                Arg::new("section")
                    .required(true)
                    .value_parser(["digikey"])
                    .help("What to remove"),
            ),
        )
}

fn fail(json_out: bool, msg: &str) -> u8 {
    if json_out {
        println!(
            "{}",
            json!({"ok": false, "error": {"code": "config.error", "message": msg}})
        );
    } else {
        eprintln!("error: {msg}");
    }
    1
}

/// Runs `cadlab config ...`.
pub fn run(m: &ArgMatches, json_out: bool) -> u8 {
    let (sub, sm) = m.subcommand().expect("subcommand required");
    let mut cfg = match UserConfig::load() {
        Ok(c) => c,
        Err(e) if sub == "remove" || sub == "digikey" => {
            eprintln!("warning: {e}; starting from empty settings");
            UserConfig::default()
        }
        Err(e) => return fail(json_out, &e.to_string()),
    };
    match sub {
        "path" => {
            let p = config::path().map(|p| p.display().to_string()).unwrap_or_default();
            if json_out {
                println!("{}", json!({"ok": true, "path": p}))
            } else {
                println!("{p}")
            }
            0
        }
        "show" => {
            let dk = cfg.digikey.as_ref().map(|d| {
                json!({
                    "client_id": d.client_id,
                    "client_secret": mask(&d.client_secret),
                    "site": d.site, "language": d.language, "currency": d.currency, "sandbox": d.sandbox,
                })
            });
            if json_out {
                println!("{}", json!({"ok": true, "path": config::path(), "digikey": dk}));
            } else {
                println!(
                    "settings: {}",
                    config::path().map(|p| p.display().to_string()).unwrap_or_default()
                );
                match &cfg.digikey {
                    Some(d) => println!(
                        "digikey: client id {}, secret {}{}{}{}",
                        d.client_id,
                        mask(&d.client_secret),
                        d.site.as_ref().map(|s| format!(", site {s}")).unwrap_or_default(),
                        d.currency
                            .as_ref()
                            .map(|s| format!(", currency {s}"))
                            .unwrap_or_default(),
                        if d.sandbox { ", sandbox" } else { "" }
                    ),
                    None => println!("digikey: not configured (`cadlab config digikey`)"),
                }
                for (var, what) in [
                    ("DIGIKEY_CLIENT_ID", "client id"),
                    ("DIGIKEY_CLIENT_SECRET", "client secret"),
                ] {
                    if std::env::var_os(var).is_some() {
                        println!("note: {var} is set and overrides the stored {what}");
                    }
                }
            }
            0
        }
        "remove" => {
            cfg.digikey = None;
            match cfg.save() {
                Ok(p) => {
                    if json_out {
                        println!("{}", json!({"ok": true, "path": p}))
                    } else {
                        println!("removed DigiKey credentials from {}", p.display())
                    }
                    0
                }
                Err(e) => fail(json_out, &e.to_string()),
            }
        }
        "digikey" => digikey(sm, cfg, json_out),
        _ => unreachable!(),
    }
}

fn prompt(label: &str, current: Option<&str>) -> std::io::Result<Option<String>> {
    let mut err = std::io::stderr();
    match current {
        Some(c) if !c.is_empty() => write!(err, "{label} [{c}]: ")?,
        _ => write!(err, "{label}: ")?,
    }
    err.flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let v = line.trim().to_string();
    Ok((!v.is_empty()).then_some(v))
}

fn digikey(m: &ArgMatches, mut cfg: UserConfig, json_out: bool) -> u8 {
    let interactive = std::io::stdin().is_terminal();
    let old = cfg.digikey.clone().unwrap_or_default();
    let mut d = DigiKeySettings { ..old.clone() };

    // Client ID: flag, else prompt (Enter keeps the stored one).
    match m.get_one::<String>("client_id") {
        Some(id) => d.client_id = id.trim().to_string(),
        None if interactive => {
            if old.client_id.is_empty() {
                eprintln!("DigiKey API app credentials (https://developer.digikey.com, \"My Apps\").");
            }
            match prompt("Client ID", Some(&old.client_id)) {
                Ok(Some(v)) => d.client_id = v,
                Ok(None) => {}
                Err(e) => return fail(json_out, &e.to_string()),
            }
        }
        None => {}
    }
    if d.client_id.is_empty() {
        return fail(
            json_out,
            "a client ID is required (--client-id, or run in a terminal to be prompted)",
        );
    }

    // Secret: hidden prompt, or the first line of stdin when not interactive.
    let secret = if interactive {
        let label = if old.client_secret.is_empty() {
            "Client secret: ".to_string()
        } else {
            "Client secret [keep stored]: ".to_string()
        };
        match rpassword::prompt_password(label) {
            Ok(s) => s.trim().to_string(),
            Err(e) => return fail(json_out, &e.to_string()),
        }
    } else {
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(_) => line.trim().to_string(),
            Err(e) => return fail(json_out, &e.to_string()),
        }
    };
    if !secret.is_empty() {
        d.client_secret = secret;
    }
    if d.client_secret.is_empty() {
        return fail(json_out, "a client secret is required");
    }
    if let Some(v) = m.get_one::<String>("site") {
        d.site = Some(v.to_ascii_uppercase());
    }
    if let Some(v) = m.get_one::<String>("currency") {
        d.currency = Some(v.to_ascii_uppercase());
    }
    if let Some(v) = m.get_one::<String>("language") {
        d.language = Some(v.to_ascii_lowercase());
    }
    if m.get_flag("sandbox") {
        d.sandbox = true;
    }

    if !m.get_flag("no_verify") {
        match verify(&d) {
            Ok(()) => {
                if !json_out {
                    eprintln!("credentials verified with DigiKey");
                }
            }
            Err(e) => {
                return fail(
                    json_out,
                    &format!("{e} (nothing saved; use --no-verify to save anyway)"),
                );
            }
        }
    }
    cfg.digikey = Some(d.clone());
    match cfg.save() {
        Ok(p) => {
            if json_out {
                println!(
                    "{}",
                    json!({"ok": true, "path": p, "client_id": d.client_id, "client_secret": mask(&d.client_secret)})
                );
            } else {
                println!("saved DigiKey credentials to {} (readable only by you)", p.display());
            }
            0
        }
        Err(e) => fail(json_out, &e.to_string()),
    }
}

#[cfg(feature = "net")]
fn verify(d: &DigiKeySettings) -> Result<(), String> {
    // Settings as given here, without environment overrides.
    let dk = cadlab::supplier::digikey::DigiKey::new(
        d.client_id.clone(),
        d.client_secret.clone(),
        if d.sandbox {
            "https://sandbox-api.digikey.com"
        } else {
            "https://api.digikey.com"
        },
        d.site.as_deref().unwrap_or("US"),
        d.language.as_deref().unwrap_or("en"),
        d.currency.as_deref().unwrap_or("USD"),
        None,
    );
    dk.verify().map_err(|e| e.to_string())
}

#[cfg(not(feature = "net"))]
fn verify(_: &DigiKeySettings) -> Result<(), String> {
    Err("this build has no network support (feature `net`)".into())
}
