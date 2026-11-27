//! Request-level input overlap and pricing tiers are resolved before rollup.
//! Reads apply a versioned, offline public rate table to projected units.
use super::{Field, Totals};
use protocol::{RequestMeter, TokenCounts, TokenModel};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::LazyLock};

const LONG_INPUT: i64 = 272_000;
const TIER_COUNT: usize = 3;

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ModelProjection {
    confirmed: BTreeMap<String, ModelTally>,
    requested: BTreeMap<String, i64>,
    fresh: Quantity,
}

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
struct ModelTally {
    tokens: Totals,
    // Standard, long, unknown. A request is projected into one band before
    // aggregation. Rate changes within a band require no request scan.
    tiers: [Billable; TIER_COUNT],
}

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
struct Quantity {
    known: i64,
    zero: i64,
    units: i128,
}

impl Quantity {
    fn count(&mut self, count: Option<i64>, sign: i64) {
        if let Some(n) = count {
            self.known += sign;
            self.zero += if n == 0 { sign } else { 0 };
            self.units += i128::from(sign) * i128::from(n);
        }
    }

    fn merge(&mut self, other: Self, sign: i64) {
        self.known += sign * other.known;
        self.zero += sign * other.zero;
        self.units += i128::from(sign) * other.units;
    }

    fn field(self) -> Field {
        Field {
            tokens: (self.known > 0).then(|| self.units.to_string()),
            reported_requests: self.known,
        }
    }
}

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
struct Billable {
    requests: i64,
    quantities: [Quantity; 4],
}

impl Billable {
    fn count(&mut self, counts: [Option<i64>; 4], sign: i64) {
        self.requests += sign;
        for (quantity, count) in self.quantities.iter_mut().zip(counts) {
            quantity.count(count, sign);
        }
    }

    fn merge(&mut self, other: Self, sign: i64) {
        self.requests += sign * other.requests;
        for (quantity, other) in self.quantities.iter_mut().zip(other.quantities) {
            quantity.merge(other, sign);
        }
    }
}

fn partition(counts: TokenCounts) -> (usize, [Option<i64>; 4]) {
    let [_, input, read, write, output, _reasoning] = counts.fields();
    let cached = i128::from(read.unwrap_or(0)) + i128::from(write.unwrap_or(0));
    let consistent = input.is_none_or(|i| cached <= i128::from(i));
    let fresh = input
        .zip(read)
        .zip(write)
        .and_then(|((i, r), w)| i.checked_sub(r)?.checked_sub(w).filter(|&n| n >= 0));
    // A known cache subtotal can prove the long band even if total input
    // is absent. The provider's minimum threshold is strictly exclusive.
    let tier = match input {
        Some(n) if n > LONG_INPUT => 1,
        Some(_) => 0,
        None if cached > i128::from(LONG_INPUT) => 1,
        None => 2,
    };
    (
        tier,
        [
            fresh,
            read.filter(|_| consistent),
            write.filter(|_| consistent),
            output,
        ],
    )
}

impl ModelTally {
    fn fresh(&self) -> Quantity {
        let mut total = Quantity::default();
        for tier in &self.tiers {
            total.merge(tier.quantities[0], 1);
        }
        total
    }
}

impl ModelProjection {
    pub(super) fn count(&mut self, meter: &RequestMeter, sign: i64) {
        let Some(model) = &meter.model else {
            // Legacy rows were never in the input split projection. A
            // subtraction during enrichment must not remove their raw split.
            return;
        };
        let (tier, counts) = partition(meter.counts);
        self.fresh.count(counts[0], sign);
        match model {
            TokenModel::Response(id) => {
                let entry = self.confirmed.entry(id.clone()).or_default();
                entry.tokens.count(meter.counts, sign);
                entry.tiers[tier].count(counts, sign);
                if entry.tokens.valid_records == 0 {
                    self.confirmed.remove(id);
                }
            }
            TokenModel::TurnContext(id) => adjust(&mut self.requested, id, sign),
            _ => {}
        }
    }

    pub(super) fn merge(&mut self, other: &Self, sign: i64) {
        self.fresh.merge(other.fresh, sign);
        for (id, tally) in &other.confirmed {
            let entry = self.confirmed.entry(id.clone()).or_default();
            entry.tokens.merge(tally.tokens, sign);
            for i in 0..TIER_COUNT {
                entry.tiers[i].merge(tally.tiers[i], sign);
            }
            // Signed deltas can enrich a request without changing the model's
            // request count. Remove only the additive identity.
            if *entry == ModelTally::default() {
                self.confirmed.remove(id);
            }
        }
        for (id, requests) in &other.requested {
            adjust(&mut self.requested, id, sign * requests);
        }
    }
}

fn adjust(entries: &mut BTreeMap<String, i64>, id: &str, delta: i64) {
    let count = entries.entry(id.to_owned()).or_default();
    *count += delta;
    if *count == 0 {
        entries.remove(id);
    }
}

#[derive(Deserialize)]
struct Catalog {
    reference_date: &'static str,
    models: Vec<Price>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Price {
    id: &'static str,
    canonical_slug: &'static str,
    standard: Rates,
    long: Option<Rates>,
    unsupported: Option<UnsupportedPrice>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum UnsupportedPrice {
    UtcSchedule,
    OtherConditions,
}

type Rates = [Option<i64>; 4];

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    serde_json::from_str(include_str!("openrouter-prices.json")).expect("bundled OpenRouter rates")
});

fn price(model: &str) -> Option<&'static Price> {
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
    // A bare name must identify one entry. Never silently select a vendor
    // when a future catalog adds another model with the same short slug.
    matches.next().filter(|_| matches.next().is_none())
}

#[derive(Clone, Serialize)]
pub(super) struct Estimate {
    usd: Option<String>,
    partial: bool,
}

#[derive(Clone, Copy, Default)]
struct Accumulator {
    priced: bool,
    partial: bool,
    pico_usd: i128,
}

impl Accumulator {
    fn merge(&mut self, other: Self) {
        self.priced |= other.priced;
        self.partial |= other.partial;
        self.pico_usd += other.pico_usd;
    }

    fn estimate(self) -> Estimate {
        Estimate {
            usd: self.priced.then(|| decimal(self.pico_usd, 12)),
            partial: self.partial,
        }
    }
}

fn decimal(value: i128, places: u32) -> String {
    let divisor = 10_i128.pow(places);
    let fract = format!("{:0width$}", value % divisor, width = places as usize);
    let fract = fract.trim_end_matches('0');
    if fract.is_empty() {
        (value / divisor).to_string()
    } else {
        format!("{}.{fract}", value / divisor)
    }
}

#[derive(Clone, Serialize)]
pub(super) struct ModelRow {
    model: Option<String>,
    response_model_requests: i64,
    requested_models: BTreeMap<String, i64>,
    reported_requests: i64,
    total_tokens: Field,
    input_tokens: Field,
    uncached_input_tokens: Field,
    cached_input_tokens: Field,
    cache_write_input_tokens: Field,
    output_tokens: Field,
    reasoning_output_tokens: Field,
    estimate: Estimate,
    price: Option<ModelPrice>,
}

#[derive(Clone, Serialize)]
struct ModelPrice {
    openrouter_id: &'static str,
    canonical_slug: &'static str,
    snapshot_date: &'static str,
    long_input_above: Option<i64>,
    unsupported: Option<UnsupportedPrice>,
    tiers: Vec<PriceTier>,
}

#[derive(Clone, Serialize)]
struct PriceTier {
    tier: &'static str,
    requests: i64,
    // Ordinary input, cache read, cache write and output in this order.
    components: [PriceComponent; 4],
}

#[derive(Clone, Serialize)]
struct PriceComponent {
    tokens: Option<String>,
    reported_requests: i64,
    usd_per_million: Option<String>,
    estimate: Estimate,
}

fn component(tally: Billable, i: usize, rate: Option<i64>) -> (PriceComponent, Accumulator) {
    let q = tally.quantities[i];
    // Zero tokens cost zero at any rate. Keep this observation even when
    // other requests in the same projected bucket have unpriceable tokens.
    let priced = if rate.is_some() { q.known } else { q.zero };
    let total = Accumulator {
        priced: priced > 0,
        partial: priced < tally.requests,
        pico_usd: i128::from(rate.unwrap_or(0)) * q.units,
    };
    (
        PriceComponent {
            tokens: q.field().tokens,
            reported_requests: q.known,
            usd_per_million: rate.map(|r| decimal(i128::from(r), 6)),
            estimate: total.estimate(),
        },
        total,
    )
}

fn quote(p: &Price, tally: &ModelTally) -> (ModelPrice, Accumulator) {
    let mut tiers = tally.tiers;
    if p.long.is_none() || p.unsupported.is_some() {
        tiers[0].merge(tally.tiers[1], 1);
        tiers[0].merge(tally.tiers[2], 1);
        tiers[1..].fill(Billable::default());
    }
    let rates = if p.unsupported.is_some() {
        // The current top-level rate is not evidence of the rate applicable
        // to these historical per-request quantities.
        [[None; 4]; 3]
    } else {
        let common = p.long.map_or(p.standard, |long| {
            std::array::from_fn(|i| (p.standard[i] == long[i]).then_some(long[i]).flatten())
        });
        [p.standard, p.long.unwrap_or(p.standard), common]
    };
    let mut total = Accumulator::default();
    let tiers = [
        if p.unsupported.is_some() {
            "unsupported"
        } else {
            "standard"
        },
        "long",
        "unknown",
    ]
    .into_iter()
    .zip(tiers)
    .zip(rates)
    .filter(|((_, tally), _)| tally.requests > 0)
    .map(|((tier, tally), rates)| PriceTier {
        tier,
        requests: tally.requests,
        components: std::array::from_fn(|i| {
            let (component, cost) = component(tally, i, rates[i]);
            total.merge(cost);
            component
        }),
    })
    .collect();
    (
        ModelPrice {
            openrouter_id: p.id,
            canonical_slug: p.canonical_slug,
            snapshot_date: CATALOG.reference_date,
            long_input_above: p
                .long
                .filter(|_| p.unsupported.is_none())
                .map(|_| LONG_INPUT),
            unsupported: p.unsupported,
            tiers,
        },
        total,
    )
}

impl ModelRow {
    fn new(
        model: Option<String>,
        tokens: Totals,
        fresh: Quantity,
        requested_models: BTreeMap<String, i64>,
        quote: Option<ModelPrice>,
        estimate: Accumulator,
    ) -> Self {
        let [
            total_tokens,
            input_tokens,
            cached_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens,
        ] = tokens.fields();
        Self {
            response_model_requests: model.as_ref().map_or(0, |_| tokens.valid_records),
            model,
            requested_models,
            uncached_input_tokens: fresh.field(),
            reported_requests: tokens.valid_records,
            total_tokens,
            input_tokens,
            cached_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens,
            price: quote,
            estimate: estimate.estimate(),
        }
    }
}

pub(super) fn rows(models: ModelProjection, totals: Totals) -> (Vec<ModelRow>, Estimate) {
    let mut rows = Vec::with_capacity(models.confirmed.len() + 1);
    let mut unknown = totals;
    let mut unknown_fresh = models.fresh;
    let mut total = Accumulator::default();
    for (id, tally) in models.confirmed {
        unknown.merge(tally.tokens, -1);
        unknown_fresh.merge(tally.fresh(), -1);
        let (price, cost) = price(&id).map_or_else(
            || {
                (
                    None,
                    Accumulator {
                        partial: true,
                        ..Accumulator::default()
                    },
                )
            },
            |p| {
                let (price, cost) = quote(p, &tally);
                (Some(price), cost)
            },
        );
        total.merge(cost);
        rows.push(ModelRow::new(
            Some(id),
            tally.tokens,
            tally.fresh(),
            BTreeMap::new(),
            price,
            cost,
        ));
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.reported_requests));
    if unknown.valid_records > 0 {
        let cost = Accumulator {
            partial: true,
            ..Accumulator::default()
        };
        total.merge(cost);
        rows.push(ModelRow::new(
            None,
            unknown,
            unknown_fresh,
            models.requested,
            None,
            cost,
        ));
    }
    (rows, total.estimate())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn measure(
        totals: &mut Totals,
        models: &mut ModelProjection,
        id: &str,
        fields: [Option<i64>; 6],
    ) {
        let meter = RequestMeter {
            counts: TokenCounts::from_fields(fields),
            model: Some(TokenModel::Response(id.to_owned())),
        };
        totals.count(meter.counts, 1);
        models.count(&meter, 1);
    }

    #[test]
    fn native_sample_uses_exclusive_categories_and_request_tier() {
        let mut totals = Totals::default();
        let mut models = ModelProjection::default();
        // Anonymised request from a legacy rollout. Raw input includes the
        // cache read. Reasoning, when present, is included in output.
        measure(
            &mut totals,
            &mut models,
            "gpt-6.2-sol",
            [30627, 30548, 29824, 0, 79, 0].map(Some),
        );
        measure(
            &mut totals,
            &mut models,
            "gpt-6.2-sol",
            [272011, 272001, 3000, 10000, 10, 3].map(Some),
        );
        measure(
            &mut totals,
            &mut models,
            "deepseek-v4.1-flash",
            [25021, 24556, 11904, 0, 465, 332].map(Some),
        );
        let (rows, estimate) = rows(models, totals);
        let rows = json!(rows);
        assert_eq!(
            rows[0]["estimate"],
            json!({"usd":"1.0919744","partial":false})
        );
        let gpt = &rows[0]["price"];
        assert_eq!(gpt["openrouter_id"], "openai/gpt-6.2-sol");
        assert_eq!(gpt["tiers"][0]["components"][0]["tokens"], "724");
        assert_eq!(
            gpt["tiers"][0]["components"][1]["estimate"]["usd"],
            "0.0029824"
        );
        assert_eq!(gpt["tiers"][1]["components"][2]["estimate"]["usd"], "0.05");
        assert_eq!(
            rows[1]["estimate"],
            json!({"usd":"0.0020165768","partial":false})
        );
        assert_eq!(
            json!(estimate),
            json!({"usd":"1.0939909768","partial":false})
        );
    }

    #[test]
    fn incomplete_counts_inconsistent_split_and_unknown_models_are_not_zero() {
        let mut totals = Totals::default();
        let mut models = ModelProjection::default();
        measure(
            &mut totals,
            &mut models,
            "gpt-6.2-sol",
            [None, Some(100), Some(20), None, Some(50), Some(25)],
        );
        measure(
            &mut totals,
            &mut models,
            "gpt-6.2-sol",
            [None, Some(100), Some(90), Some(20), Some(0), None],
        );
        measure(
            &mut totals,
            &mut models,
            "gpt-6.2-sol",
            [None, None, Some(10), Some(5), Some(20), None],
        );
        measure(
            &mut totals,
            &mut models,
            "gpt-6.2-sol:batch",
            [None, Some(500), Some(300), Some(0), Some(23), None],
        );
        totals.count(
            TokenCounts {
                total_tokens: Some(999),
                ..TokenCounts::default()
            },
            1,
        );
        let (rows, estimate) = rows(models, totals);
        let rows = json!(rows);
        assert_eq!(
            rows[0]["price"]["tiers"][0]["components"][0]["tokens"],
            json!(null)
        );
        assert_eq!(
            rows[0]["price"]["tiers"][0]["components"][1]["tokens"],
            "20"
        );
        assert_eq!(
            rows[0]["price"]["tiers"][1]["components"][3]["estimate"]["usd"],
            json!(null)
        );
        assert_eq!(rows[1]["price"], json!(null));
        assert_eq!(rows[2]["total_tokens"]["tokens"], "999");
        assert_eq!(json!(estimate), json!({"usd":"0.000502","partial":true}));
    }

    #[test]
    fn equality_remains_standard_and_unresolved_utc_never_uses_top_level_rates() {
        let mut totals = Totals::default();
        let mut models = ModelProjection::default();
        measure(
            &mut totals,
            &mut models,
            "gpt-6.2-sol",
            [272010, 272000, 3000, 10000, 10, 3].map(Some),
        );
        measure(
            &mut totals,
            &mut models,
            "deepseek-v4.1-flash-1017",
            [101, 100, 20, 1, 1, 0].map(Some),
        );
        let (rows, estimate) = rows(models, totals);
        let rows = json!(rows);
        assert_eq!(rows[1]["price"]["tiers"][0]["tier"], "standard");
        assert_eq!(rows[1]["price"]["long_input_above"], 272000);
        assert_eq!(rows[0]["price"]["unsupported"], "utc_schedule");
        assert_eq!(rows[0]["estimate"], json!({"usd":null,"partial":true}));
        assert_eq!(json!(estimate), json!({"usd":"0.5434","partial":true}));
    }
}
