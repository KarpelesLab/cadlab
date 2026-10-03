//! Live DigiKey test. Runs only with `DIGIKEY_CLIENT_ID` and `DIGIKEY_CLIENT_SECRET` set; skipped
//! (passes with a message) otherwise. Uses the real API and the user cache.

#![cfg(feature = "net")]

use cadlab::supplier::digikey::DigiKey;
use cadlab::supplier::{Provider, SearchQuery};

#[test]
fn digikey_live_lookup_and_search() {
    let Some(dk) = DigiKey::from_env() else {
        eprintln!("skipping: DIGIKEY_CLIENT_ID / DIGIKEY_CLIENT_SECRET not set");
        return;
    };
    let found = dk.lookup("AP2112K-3.3TRG1").expect("lookup");
    assert!(!found.is_empty(), "AP2112K-3.3TRG1 should be listed");
    let c = &found[0];
    eprintln!("{c:#?}");
    assert_eq!(c.manufacturer.as_deref().map(|m| m.contains("Diodes")), Some(true));
    assert!(!c.prices.is_empty(), "price breaks parsed");
    assert!(
        c.params.get("voltage_out").is_some(),
        "parameters normalized: {:?}",
        c.params
    );

    let q = SearchQuery {
        text: "LDO".into(),
        package: Some("SOT-23-5".into()),
        in_stock: true,
        ..Default::default()
    };
    let results: Vec<_> = dk
        .search(&q)
        .expect("search")
        .into_iter()
        .filter(|c| q.matches(c))
        .collect();
    eprintln!("{} matching candidates", results.len());
    assert!(!results.is_empty(), "keyword search returns usable candidates");
}
