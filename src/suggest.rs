//! "Did you mean" suggestions for unknown names.

/// Returns up to `max` candidates similar to `input`, best first. Comparison is
/// case-insensitive; exact case-insensitive matches always come first.
pub fn did_you_mean<'a, I>(input: &str, candidates: I, max: usize) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let needle = input.to_lowercase();
    let mut scored: Vec<(f64, &str)> = candidates
        .into_iter()
        .filter_map(|c| {
            let lc = c.to_lowercase();
            let score = if lc == needle {
                2.0
            } else {
                let jw = strsim::jaro_winkler(&needle, &lc);
                // Prefix matches are very likely what was meant (`VBU` -> `VBUS`).
                if lc.starts_with(&needle) || needle.starts_with(&lc) { jw + 0.5 } else { jw }
            };
            (score >= 0.8).then_some((score, c))
        })
        .collect();
    // Stable, deterministic: score desc, then name asc.
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    scored.dedup_by(|a, b| a.1 == b.1);
    // Only keep candidates nearly as good as the best one.
    let best = scored.first().map_or(0.0, |s| s.0);
    scored.into_iter().take_while(|s| s.0 >= best - 0.1).take(max).map(|(_, c)| c.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_close_names() {
        let names = ["VBUS", "GND", "VCC_3V3", "USB_D+", "USB_D-"];
        assert_eq!(did_you_mean("vbus", names, 3), vec!["VBUS"]);
        assert_eq!(did_you_mean("VBU", names, 3)[0], "VBUS");
        assert!(did_you_mean("XYZZY", names, 3).is_empty());
        assert_eq!(did_you_mean("USB_D", names, 2), vec!["USB_D+", "USB_D-"]);
    }
}
