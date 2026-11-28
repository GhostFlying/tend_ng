//! The same offline catalog is used for identity corroboration and quoting.
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Deserialize)]
struct Catalog {
    reference_date: &'static str,
    models: Vec<Price>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub id: &'static str,
    pub canonical_slug: &'static str,
    pub input_billing: InputBilling,
    pub standard: Rates,
    pub long: Option<Rates>,
    pub unsupported: Option<UnsupportedPrice>,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputBilling {
    HitMiss,
    IncludedWrite,
    SeparateWrite,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedPrice {
    UtcSchedule,
    OtherConditions,
}

pub type Rates = [Option<i64>; 4];

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    serde_json::from_str(include_str!("openrouter-prices.json")).expect("bundled OpenRouter rates")
});

pub fn snapshot_date() -> &'static str {
    CATALOG.reference_date
}

pub fn price(model: &str) -> Option<&'static Price> {
    let mut matches = CATALOG.models.iter().filter(|item| {
        model == item.id
            || model == item.canonical_slug
            || item
                .id
                .split_once('/')
                .is_some_and(|(_, slug)| slug == model)
            || item
                .canonical_slug
                .split_once('/')
                .is_some_and(|(_, slug)| slug == model)
    });
    // A short name must identify one catalog entry. Do not guess the vendor.
    matches.next().filter(|_| matches.next().is_none())
}
