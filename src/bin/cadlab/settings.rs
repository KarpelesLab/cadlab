//! `cadlab config`: user settings (supplier credentials). CLI-only on purpose: secrets are typed
//! by the user, never passed through an agent or MCP (DECISIONS D17).

use std::io::{BufRead, IsTerminal, Write};

use cadlab::config::{self, DigiKeySettings, MouserSettings, NexarSettings, UserConfig, mask};
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
                .arg(Arg::new("site").long("site").value_name("SITE").help("Locale site: US, DE, UK, ... (default US)"))
                .arg(
                    Arg::new("currency")
                        .long("currency")
                        .value_name("CUR")
                        .help("Price currency: USD, EUR, ... (default USD)"),
                )
                .arg(Arg::new("language").long("language").value_name("LANG").help("Language (default en)"))
                .arg(Arg::new("sandbox").long("sandbox").action(ArgAction::SetTrue).help("Use DigiKey's sandbox API"))
                .arg(
                    Arg::new("no_verify")
                        .long("no-verify")
                        .action(ArgAction::SetTrue)
                        .help("Save without checking the credentials against DigiKey"),
                ),
        )
        .subcommand(
            Command::new("mouser")
                .about("Set the Mouser Search API key (prompts for it without echo; verifies it before saving)")
                .long_about(
                    "Set the Mouser Search API key (request one at https://www.mouser.com/api-search/).\n\
                     Prompts for the key without echo; press Enter to keep a stored key. Without a terminal, the\n\
                     key is read from the first line of standard input: printf '%s\\n' \"$KEY\" | cadlab config mouser",
                )
                .arg(no_verify("Mouser")),
        )
        .subcommand(
            Command::new("nexar")
                .about(
                    "Set Nexar (Octopart) API credentials (prompts for what is not given; verifies them before saving)",
                )
                .long_about(
                    "Set Nexar (Octopart) API credentials: the client ID and secret of an application with the\n\
                     Supply scope (https://portal.nexar.com). Prompts for the client ID and, without echo, the\n\
                     secret; press Enter to keep a stored value. Without a terminal, the secret is read from the\n\
                     first line of standard input: printf '%s\\n' \"$SECRET\" | cadlab config nexar --client-id ID",
                )
                .arg(Arg::new("client_id").long("client-id").value_name("ID").help("Client ID of your Nexar app"))
                .arg(
                    Arg::new("country")
                        .long("country")
                        .value_name("CC")
                        .help("Country for offers, ISO 3166 alpha-2 (default US)"),
                )
                .arg(
                    Arg::new("currency")
                        .long("currency")
                        .value_name("CUR")
                        .help("Currency prices are converted to (default USD)"),
                )
                .arg(
                    Arg::new("unauthorized")
                        .long("unauthorized")
                        .action(ArgAction::SetTrue)
                        .help("Also list offers from sellers not authorized by the manufacturer (never brokers)"),
                )
                .arg(no_verify("Nexar")),
        )
        .subcommand(Command::new("remove").about("Remove stored credentials").arg(
            Arg::new("section").required(true).value_parser(["digikey", "mouser", "nexar"]).help("What to remove"),
        ))
}

fn no_verify(who: &str) -> Arg {
    Arg::new("no_verify")
        .long("no-verify")
        .action(ArgAction::SetTrue)
        .help(format!("Save without checking the credentials against {who}"))
}

fn fail(json_out: bool, msg: &str) -> u8 {
    if json_out {
        println!("{}", json!({"ok": false, "error": {"code": "config.error", "message": msg}}));
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
        Err(e) if sub != "path" && sub != "show" => {
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
            let mouser = cfg.mouser.as_ref().map(|m| json!({"api_key": mask(&m.api_key)}));
            let nexar = cfg.nexar.as_ref().map(|n| {
                json!({
                    "client_id": n.client_id, "client_secret": mask(&n.client_secret),
                    "country": n.country, "currency": n.currency, "unauthorized": n.unauthorized,
                })
            });
            if json_out {
                println!(
                    "{}",
                    json!({"ok": true, "path": config::path(), "digikey": dk, "mouser": mouser, "nexar": nexar,
                           "catalogs": cadlab::supplier::catalog_dir(),
                           "user_library": cadlab::userlib::user_library_dir(), "libraries": cfg.library_paths()})
                );
            } else {
                println!("settings: {}", config::path().map(|p| p.display().to_string()).unwrap_or_default());
                match &cfg.digikey {
                    Some(d) => println!(
                        "digikey: client id {}, secret {}{}{}{}",
                        d.client_id,
                        mask(&d.client_secret),
                        d.site.as_ref().map(|s| format!(", site {s}")).unwrap_or_default(),
                        d.currency.as_ref().map(|s| format!(", currency {s}")).unwrap_or_default(),
                        if d.sandbox { ", sandbox" } else { "" }
                    ),
                    None => println!("digikey: not configured (`cadlab config digikey`)"),
                }
                match &cfg.mouser {
                    Some(m) => println!("mouser: API key {}", mask(&m.api_key)),
                    None => println!("mouser: not configured (`cadlab config mouser`)"),
                }
                match &cfg.nexar {
                    Some(n) => println!(
                        "nexar: client id {}, secret {}{}{}{}",
                        n.client_id,
                        mask(&n.client_secret),
                        n.country.as_ref().map(|s| format!(", country {s}")).unwrap_or_default(),
                        n.currency.as_ref().map(|s| format!(", currency {s}")).unwrap_or_default(),
                        if n.unauthorized { ", unauthorized sellers included" } else { "" }
                    ),
                    None => println!("nexar: not configured (`cadlab config nexar`)"),
                }
                if let Some(d) = cadlab::supplier::catalog_dir() {
                    println!("catalogs: {} (`cadlab catalog import`)", d.display());
                }
                let user = cadlab::userlib::user_library_dir().map(|p| p.display().to_string()).unwrap_or_default();
                println!("user library: {user}");
                for l in cfg.library_paths() {
                    println!("library: {} (from `libraries` in the settings file)", l.display());
                }
                for (var, what) in [
                    ("DIGIKEY_CLIENT_ID", "DigiKey client id"),
                    ("DIGIKEY_CLIENT_SECRET", "DigiKey client secret"),
                    ("MOUSER_API_KEY", "Mouser API key"),
                    ("NEXAR_CLIENT_ID", "Nexar client id"),
                    ("NEXAR_CLIENT_SECRET", "Nexar client secret"),
                ] {
                    if std::env::var_os(var).is_some() {
                        println!("note: {var} is set and overrides the stored {what}");
                    }
                }
            }
            0
        }
        "remove" => {
            let section = sm.get_one::<String>("section").map(String::as_str).unwrap_or("");
            let name = match section {
                "mouser" => {
                    cfg.mouser = None;
                    "the Mouser API key"
                }
                "nexar" => {
                    cfg.nexar = None;
                    "Nexar credentials"
                }
                _ => {
                    cfg.digikey = None;
                    "DigiKey credentials"
                }
            };
            match cfg.save() {
                Ok(p) => {
                    if json_out {
                        println!("{}", json!({"ok": true, "path": p}))
                    } else {
                        println!("removed {name} from {}", p.display())
                    }
                    0
                }
                Err(e) => fail(json_out, &e.to_string()),
            }
        }
        "digikey" => digikey(sm, cfg, json_out),
        "mouser" => mouser(sm, cfg, json_out),
        "nexar" => nexar(sm, cfg, json_out),
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

/// A secret: hidden prompt, or the first line of stdin when not interactive. Empty: keep the
/// stored one.
fn read_secret(label: &str, stored: bool, interactive: bool) -> std::io::Result<String> {
    if interactive {
        let label = if stored { format!("{label} [keep stored]: ") } else { format!("{label}: ") };
        rpassword::prompt_password(label).map(|s| s.trim().to_string())
    } else {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        Ok(line.trim().to_string())
    }
}

fn saved(json_out: bool, p: &std::path::Path, what: &str, extra: serde_json::Value) -> u8 {
    if json_out {
        let mut v = json!({"ok": true, "path": p});
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        println!("{v}");
    } else {
        println!("saved {what} to {} (readable only by you)", p.display());
    }
    0
}

fn mouser(m: &ArgMatches, mut cfg: UserConfig, json_out: bool) -> u8 {
    let interactive = std::io::stdin().is_terminal();
    let old = cfg.mouser.clone().unwrap_or_default();
    if interactive && old.api_key.is_empty() {
        eprintln!("Mouser Search API key (request one at https://www.mouser.com/api-search/).");
    }
    let key = match read_secret("Search API key", !old.api_key.is_empty(), interactive) {
        Ok(k) if k.is_empty() => old.api_key.clone(),
        Ok(k) => k,
        Err(e) => return fail(json_out, &e.to_string()),
    };
    if key.is_empty() {
        return fail(json_out, "an API key is required (typed at the prompt, or the first line of standard input)");
    }
    let s = MouserSettings { api_key: key };
    if !m.get_flag("no_verify") {
        match verify_mouser(&s) {
            Ok(()) if !json_out => eprintln!("API key verified with Mouser"),
            Ok(()) => {}
            Err(e) => return fail(json_out, &format!("{e} (nothing saved; use --no-verify to save anyway)")),
        }
    }
    cfg.mouser = Some(s.clone());
    match cfg.save() {
        Ok(p) => saved(json_out, &p, "the Mouser API key", json!({"api_key": mask(&s.api_key)})),
        Err(e) => fail(json_out, &e.to_string()),
    }
}

fn nexar(m: &ArgMatches, mut cfg: UserConfig, json_out: bool) -> u8 {
    let interactive = std::io::stdin().is_terminal();
    let old = cfg.nexar.clone().unwrap_or_default();
    let mut n = old.clone();
    match m.get_one::<String>("client_id") {
        Some(id) => n.client_id = id.trim().to_string(),
        None if interactive => {
            if old.client_id.is_empty() {
                eprintln!("Nexar application credentials (https://portal.nexar.com, an app with the Supply scope).");
            }
            match prompt("Client ID", Some(&old.client_id)) {
                Ok(Some(v)) => n.client_id = v,
                Ok(None) => {}
                Err(e) => return fail(json_out, &e.to_string()),
            }
        }
        None => {}
    }
    if n.client_id.is_empty() {
        return fail(json_out, "a client ID is required (--client-id, or run in a terminal to be prompted)");
    }
    match read_secret("Client secret", !old.client_secret.is_empty(), interactive) {
        Ok(s) if !s.is_empty() => n.client_secret = s,
        Ok(_) => {}
        Err(e) => return fail(json_out, &e.to_string()),
    }
    if n.client_secret.is_empty() {
        return fail(json_out, "a client secret is required");
    }
    if let Some(v) = m.get_one::<String>("country") {
        n.country = Some(v.to_ascii_uppercase());
    }
    if let Some(v) = m.get_one::<String>("currency") {
        n.currency = Some(v.to_ascii_uppercase());
    }
    if m.get_flag("unauthorized") {
        n.unauthorized = true;
    }
    if !m.get_flag("no_verify") {
        match verify_nexar(&n) {
            Ok(()) if !json_out => eprintln!("credentials verified with Nexar"),
            Ok(()) => {}
            Err(e) => return fail(json_out, &format!("{e} (nothing saved; use --no-verify to save anyway)")),
        }
    }
    cfg.nexar = Some(n.clone());
    match cfg.save() {
        Ok(p) => saved(
            json_out,
            &p,
            "Nexar credentials",
            json!({"client_id": n.client_id, "client_secret": mask(&n.client_secret)}),
        ),
        Err(e) => fail(json_out, &e.to_string()),
    }
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
        return fail(json_out, "a client ID is required (--client-id, or run in a terminal to be prompted)");
    }

    let secret = match read_secret("Client secret", !old.client_secret.is_empty(), interactive) {
        Ok(s) => s,
        Err(e) => return fail(json_out, &e.to_string()),
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
                return fail(json_out, &format!("{e} (nothing saved; use --no-verify to save anyway)"));
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
        if d.sandbox { "https://sandbox-api.digikey.com" } else { "https://api.digikey.com" },
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

#[cfg(feature = "net")]
fn verify_mouser(s: &MouserSettings) -> Result<(), String> {
    use cadlab::supplier::{http::Ureq, mouser};
    let m = mouser::Mouser::new(s.api_key.clone(), mouser::BASE, std::sync::Arc::new(Ureq::default()), None);
    m.verify().map_err(|e| e.to_string())
}

#[cfg(not(feature = "net"))]
fn verify_mouser(_: &MouserSettings) -> Result<(), String> {
    Err("this build has no network support (feature `net`)".into())
}

#[cfg(feature = "net")]
fn verify_nexar(s: &NexarSettings) -> Result<(), String> {
    use cadlab::supplier::{http::Ureq, nexar::Nexar};
    let n = Nexar::new(
        s.client_id.clone(),
        s.client_secret.clone(),
        s.country.as_deref().unwrap_or("US"),
        s.currency.as_deref().unwrap_or("USD"),
        std::sync::Arc::new(Ureq::default()),
        None,
    );
    n.verify().map_err(|e| e.to_string())
}

#[cfg(not(feature = "net"))]
fn verify_nexar(_: &NexarSettings) -> Result<(), String> {
    Err("this build has no network support (feature `net`)".into())
}
