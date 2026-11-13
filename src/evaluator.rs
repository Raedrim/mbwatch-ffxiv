//! Pure alert-rule evaluation (no I/O).

use crate::config::WatchItem;
use crate::universalis::ItemAggregate;

#[derive(Debug, Clone, PartialEq)]
pub enum Reason {
    /// Cheapest price fell below the `alert_below` threshold.
    Below { threshold: u64 },
    /// Cheapest DC price is at least `margin` cheaper than the home world.
    Arbitrage { margin: f64, savings_pct: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub item_id: u32,
    pub item_name: String,
    pub hq: bool,
    /// World holding the cheapest listing.
    pub world_id: u32,
    pub world_name: Option<String>,
    pub price: u64,
    pub home_price: Option<u64>,
    pub reasons: Vec<Reason>,
}

/// Fraction by which `dc_price` undercuts `home_price` (`(home - dc) / home`).
pub fn savings_fraction(home_price: f64, dc_price: f64) -> f64 {
    if home_price <= 0.0 {
        return 0.0;
    }
    (home_price - dc_price) / home_price
}

/// Evaluate one watch rule against Stage 1 stats. `home_world_id` is only a
/// fallback when the API omits the cheapest listing's world ID.
pub fn evaluate(
    item: &WatchItem,
    item_id: u32,
    stats: &ItemAggregate,
    home_world_id: u32,
) -> Option<Alert> {
    let q = stats.quality(item.hq)?;
    let home = q.min_listing.world;
    let dc = q.min_listing.dc;
    // The DC minimum already includes the home world; fall back to the world price.
    let cheapest = dc.or(home)?;
    let price = cheapest.price.round() as u64;

    let mut reasons = Vec::new();
    if let Some(threshold) = item.alert_below {
        if price < threshold {
            reasons.push(Reason::Below { threshold });
        }
    }
    if let (Some(margin), Some(h), Some(d)) = (item.arbitrage_margin, home, dc) {
        let frac = savings_fraction(h.price, d.price);
        if frac >= margin {
            reasons.push(Reason::Arbitrage { margin, savings_pct: frac * 100.0 });
        }
    }
    if reasons.is_empty() {
        return None;
    }
    Some(Alert {
        item_id,
        item_name: item.item.clone(),
        hq: item.hq,
        world_id: cheapest.world_id.unwrap_or(home_world_id),
        world_name: None,
        price,
        home_price: home.map(|h| h.price.round() as u64),
        reasons,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn watch(hq: bool, below: Option<u64>, margin: Option<f64>) -> WatchItem {
        WatchItem { item: "Moqueca".into(), hq, alert_below: below, arbitrage_margin: margin }
    }

    fn stats(v: serde_json::Value) -> ItemAggregate {
        serde_json::from_value(v).unwrap()
    }

    fn both() -> ItemAggregate {
        stats(json!({"itemId": 1,
          "nq": {"minListing": {"world": {"price": 100, "worldId": 33}, "dc": {"price": 90, "worldId": 21}}},
          "hq": {"minListing": {"world": {"price": 2000, "worldId": 33}, "dc": {"price": 1500, "worldId": 21}}}}))
    }

    #[test]
    fn below_threshold_triggers_with_cheapest_dc_world() {
        let a = evaluate(&watch(true, Some(1800), None), 1, &both(), 33).unwrap();
        assert_eq!((a.price, a.world_id, a.hq, a.item_id), (1500, 21, true, 1));
        assert_eq!(a.home_price, Some(2000));
        assert_eq!(a.reasons, vec![Reason::Below { threshold: 1800 }]);
    }

    #[test]
    fn at_or_above_threshold_does_not_trigger() {
        assert!(evaluate(&watch(true, Some(1500), None), 1, &both(), 33).is_none());
        assert!(evaluate(&watch(true, Some(1000), None), 1, &both(), 33).is_none());
    }

    #[test]
    fn hq_and_nq_are_isolated() {
        // NQ is dirt cheap but we watch HQ with threshold 50 -> no alert.
        assert!(evaluate(&watch(true, Some(50), None), 1, &both(), 33).is_none());
        // Watching NQ with threshold 95 -> NQ dc price 90 triggers.
        let a = evaluate(&watch(false, Some(95), None), 1, &both(), 33).unwrap();
        assert_eq!((a.price, a.hq), (90, false));
    }

    #[test]
    fn missing_quality_block_yields_none() {
        let s = stats(json!({"itemId": 1, "nq": {"minListing": {"dc": {"price": 1, "worldId": 2}}}}));
        assert!(evaluate(&watch(true, Some(1_000_000), None), 1, &s, 33).is_none());
    }

    #[test]
    fn arbitrage_math_is_inclusive_at_margin() {
        // (2000-1500)/2000 = 0.25 exactly
        let a = evaluate(&watch(true, None, Some(0.25)), 1, &both(), 33).unwrap();
        match &a.reasons[0] {
            Reason::Arbitrage { margin, savings_pct } => {
                assert_eq!(*margin, 0.25);
                assert!((savings_pct - 25.0).abs() < 1e-9);
            }
            r => panic!("{r:?}"),
        }
        assert!(evaluate(&watch(true, None, Some(0.26)), 1, &both(), 33).is_none());
    }

    #[test]
    fn savings_fraction_formula() {
        assert!((savings_fraction(2000.0, 1500.0) - 0.25).abs() < 1e-12);
        assert_eq!(savings_fraction(0.0, 5.0), 0.0); // guard against division by zero
        assert!(savings_fraction(100.0, 150.0) < 0.0);
    }

    #[test]
    fn both_rules_produce_one_alert_with_two_reasons() {
        let a = evaluate(&watch(true, Some(1800), Some(0.2)), 1, &both(), 33).unwrap();
        assert_eq!(a.reasons.len(), 2);
    }

    #[test]
    fn arbitrage_needs_both_world_and_dc_prices() {
        let s = stats(json!({"itemId": 1, "hq": {"minListing": {"dc": {"price": 10, "worldId": 2}}}}));
        assert!(evaluate(&watch(true, None, Some(0.1)), 1, &s, 33).is_none());
    }

    #[test]
    fn falls_back_to_world_price_when_dc_missing() {
        let s = stats(json!({"itemId": 1, "hq": {"minListing": {"world": {"price": 10, "worldId": 33}}}}));
        let a = evaluate(&watch(true, Some(100), None), 1, &s, 33).unwrap();
        assert_eq!((a.price, a.world_id), (10, 33));
    }

    #[test]
    fn missing_world_id_falls_back_to_home_world() {
        let s = stats(json!({"itemId": 1, "hq": {"minListing": {"dc": {"price": 10}}}}));
        let a = evaluate(&watch(true, Some(100), None), 1, &s, 77).unwrap();
        assert_eq!(a.world_id, 77);
    }
}
