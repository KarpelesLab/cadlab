//! Live Mouser and Nexar tests. Each runs only when its credentials are in the environment
//! (`MOUSER_API_KEY`; `NEXAR_CLIENT_ID` and `NEXAR_CLIENT_SECRET`) and passes with a message
//! otherwise; stored settings are not used, so a plain `cargo test` spends no API quota. Uses the
//! real APIs and the user cache.

#![cfg(feature = "net")]

use cadlab::supplier::mouser::Mouser;
use cadlab::supplier::nexar::Nexar;
use cadlab::supplier::{Provider, SearchQuery};

fn ldo_query() -> SearchQuery {
    SearchQuery { text: "LDO".into(), package: Some("SOT-23-5".into()), in_stock: true, ..Default::default() }
}

#[test]
fn mouser_live_lookup_and_search() {
    let Some(m) = Mouser::from_settings(None) else {
        eprintln!("skipping: MOUSER_API_KEY not set");
        return;
    };
    let found = m.lookup("AP2112K-3.3TRG1").expect("lookup");
    assert!(!found.is_empty(), "AP2112K-3.3TRG1 should be listed");
    eprintln!("{:#?}", found[0]);
    assert!(!found[0].prices.is_empty(), "price breaks parsed");
    let q = ldo_query();
    let n = m.search(&q).expect("search").into_iter().filter(|c| q.matches(c)).count();
    eprintln!("{n} matching candidates");
}

#[test]
fn nexar_live_lookup_and_search() {
    let Some(nx) = Nexar::from_settings(None) else {
        eprintln!("skipping: NEXAR_CLIENT_ID / NEXAR_CLIENT_SECRET not set");
        return;
    };
    let found = nx.lookup("AP2112K-3.3TRG1").expect("lookup");
    assert!(!found.is_empty(), "AP2112K-3.3TRG1 should have offers");
    eprintln!("{:#?}", found[0]);
    let q = ldo_query();
    let n = nx.search(&q).expect("search").into_iter().filter(|c| q.matches(c)).count();
    eprintln!("{n} matching candidates");
}
