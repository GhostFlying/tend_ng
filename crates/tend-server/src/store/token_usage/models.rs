//! Request-level input overlap and pricing tiers are resolved before rollup.
//! Reads apply a versioned, offline public rate table to projected units.
use super::{Field, Totals};
use protocol::{
    RequestMeter, TokenCounts, TokenModel,
    token_prices::{InputBilling, Price, UnsupportedPrice, price, snapshot_date},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const LONG_INPUT: i64 = 272_000;
const TIER_COUNT: usize = 3;

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ModelProjection {
    confirmed: BTreeMap<String, ModelTally>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    selected: BTreeMap<String, ModelTally>,
    fresh: Quantity,
}

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
struct ModelTally {
    tokens: Totals,
    #[serde(default, skip_serializing_if = "is_zero")]
    request_level_selections: i64,
    // Standard, long, unknown. A request is projected into one band before
    // aggregation. Rate changes within a band require no request scan.
    tiers: [Billable; TIER_COUNT],
}

fn is_zero(value: &i64) -> bool {
    *value == 0
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

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
struct Billable {
    requests: i64,
    quantities: [Quantity; 4],
    #[serde(default)]
    baseline: Quantity,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    notes: BTreeMap<PricingNote, i64>,
}

#[derive(Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum PricingNote {
    ReadDiscountUnobserved,
    WriteAdjustmentUnobserved,
    ConflictingInput,
    UnexpectedWrite,
}

impl Billable {
    fn count(&mut self, split: Split, sign: i64) {
        self.requests += sign;
        for (quantity, count) in self.quantities.iter_mut().zip(split.quantities) {
            quantity.count(count, sign);
        }
        self.baseline.count(split.baseline, sign);
        for note in split.notes.into_iter().flatten() {
            self.add_note(note, sign);
        }
    }

    fn add_note(&mut self, note: PricingNote, delta: i64) {
        let count = self.notes.entry(note).or_default();
        *count += delta;
        if *count == 0 {
            self.notes.remove(&note);
        }
    }

    fn merge(&mut self, other: &Self, sign: i64) {
        self.requests += sign * other.requests;
        for (quantity, other) in self.quantities.iter_mut().zip(other.quantities) {
            quantity.merge(other, sign);
        }
        self.baseline.merge(other.baseline, sign);
        for (&note, &count) in &other.notes {
            self.add_note(note, sign * count);
        }
    }
}

#[derive(Clone, Copy)]
struct Split {
    tier: usize,
    quantities: [Option<i64>; 4],
    baseline: Option<i64>,
    notes: [Option<PricingNote>; 2],
}

fn partition(counts: TokenCounts, billing: Option<InputBilling>) -> Split {
    let [_, input, read, write, output, _reasoning] = counts.fields();
    let hit_miss = billing == Some(InputBilling::HitMiss);
    let included = billing == Some(InputBilling::IncludedWrite);
    let cached = i128::from(read.unwrap_or(0))
        + if hit_miss {
            0
        } else {
            i128::from(write.unwrap_or(0))
        };
    let tier_min = if hit_miss {
        cached + i128::from(counts.cache_miss_input_tokens.unwrap_or(0))
    } else {
        cached
    };
    let tier = match input {
        Some(n) if n > LONG_INPUT => 1,
        Some(_) => 0,
        None if tier_min > i128::from(LONG_INPUT) => 1,
        None => 2,
    };
    let mut split = Split {
        tier,
        quantities: [None, None, None, output],
        baseline: None,
        notes: [None; 2],
    };
    if hit_miss {
        let explicit = counts.cache_miss_input_tokens;
        let derived = input
            .zip(read)
            .and_then(|(i, r)| i.checked_sub(r).filter(|&n| n >= 0));
        // Explicit disjoint quantities take precedence over a disagreement
        // with the inclusive total. Keep the disagreement visible.
        let inconsistent = input.is_some_and(|i| {
            tier_min > i128::from(i)
                || (read.is_some() && explicit.is_some() && tier_min != i128::from(i))
        });
        split.quantities[0] = explicit.or(derived);
        split.quantities[1] = read.filter(|_| !inconsistent || explicit.is_some());
        split.notes = [
            inconsistent.then_some(PricingNote::ConflictingInput),
            write
                .is_some_and(|w| w > 0)
                .then_some(PricingNote::UnexpectedWrite),
        ];
    } else if input.is_some_and(|i| cached > i128::from(i)) {
        split.notes[0] = Some(PricingNote::ConflictingInput);
    } else {
        split.quantities[1] = read;
        split.quantities[2] = if included { None } else { write };
        if let Some(input) = input {
            let remainder =
                input - read.unwrap_or(0) - if included { 0 } else { write.unwrap_or(0) };
            let missing_read = read.is_none();
            let missing_write = !included && write.is_none();
            if !missing_read && !missing_write {
                split.quantities[0] = Some(remainder);
            } else if billing.is_some() && remainder > 0 {
                split.baseline = Some(remainder);
                split.notes = [
                    missing_read.then_some(PricingNote::ReadDiscountUnobserved),
                    missing_write.then_some(PricingNote::WriteAdjustmentUnobserved),
                ];
            }
        }
    }
    split
}

impl ModelTally {
    fn count(&mut self, counts: TokenCounts, split: Split, sign: i64) {
        self.tokens.count(counts, sign);
        self.tiers[split.tier].count(split, sign);
    }

    fn merge(&mut self, other: &Self, sign: i64) {
        self.tokens.merge(other.tokens, sign);
        self.request_level_selections += sign * other.request_level_selections;
        for (tier, other) in self.tiers.iter_mut().zip(&other.tiers) {
            tier.merge(other, sign);
        }
    }

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
        let id = match model.basis() {
            TokenModel::Response(id)
            | TokenModel::TurnContext(id)
            | TokenModel::RequestSelection(id) => Some(id.as_str()),
            _ => None,
        };
        // Older writers could persist runtime-default optional zeros.
        // Normalize only the quantities used for estimation. The observed
        // ledger and raw usage summaries must retain their original counts.
        let billable = if meter.provider_counts {
            meter.counts
        } else {
            meter.counts.without_unproven_optional_zeros()
        };
        let split = partition(billable, id.and_then(price).map(|p| p.input_billing));
        self.fresh.count(split.quantities[0], sign);
        if let TokenModel::Response(id) = model.basis() {
            count_model(&mut self.confirmed, id, meter.counts, split, sign, false);
        } else {
            self.count_selection(meter, split, sign);
        }
    }

    fn count_selection(&mut self, meter: &RequestMeter, split: Split, sign: i64) {
        let model = meter.model.as_ref().map(TokenModel::basis);
        if let Some(TokenModel::TurnContext(id) | TokenModel::RequestSelection(id)) = model {
            count_model(
                &mut self.selected,
                id,
                meter.counts,
                split,
                sign,
                matches!(model, Some(TokenModel::RequestSelection(_))),
            );
        }
    }

    pub(super) fn merge(&mut self, other: &Self, sign: i64) {
        self.fresh.merge(other.fresh, sign);
        for (entries, other) in [
            (&mut self.confirmed, &other.confirmed),
            (&mut self.selected, &other.selected),
        ] {
            for (id, tally) in other {
                let entry = entries.entry(id.clone()).or_default();
                entry.merge(tally, sign);
                // Signed deltas can enrich the fields of a request without
                // changing the request count.
                if *entry == ModelTally::default() {
                    entries.remove(id);
                }
            }
        }
    }
}

fn count_model(
    entries: &mut BTreeMap<String, ModelTally>,
    id: &str,
    counts: TokenCounts,
    split: Split,
    sign: i64,
    request_level: bool,
) {
    let entry = entries.entry(id.to_owned()).or_default();
    entry.count(counts, split, sign);
    if request_level {
        entry.request_level_selections += sign;
    }
    if *entry == ModelTally::default() {
        entries.remove(id);
    }
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
    selected_model_requests: i64,
    request_level_selected_model_requests: i64,
    // Older Web bundles expect this property when opening row details.
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
    estimate_gap: Option<&'static str>,
}

#[derive(Clone, Serialize)]
struct ModelPrice {
    openrouter_id: &'static str,
    canonical_slug: &'static str,
    snapshot_date: &'static str,
    long_input_above: Option<i64>,
    unsupported: Option<UnsupportedPrice>,
    input_billing: InputBilling,
    tiers: Vec<PriceTier>,
}

#[derive(Clone, Serialize)]
struct PriceTier {
    tier: &'static str,
    requests: i64,
    // Ordinary input, cache read, cache write and output in this order.
    components: [PriceComponent; 4],
    baseline_input: Option<PriceComponent>,
    notes: BTreeMap<PricingNote, i64>,
}

#[derive(Clone, Serialize)]
struct PriceComponent {
    tokens: Option<String>,
    reported_requests: i64,
    usd_per_million: Option<String>,
    priced_requests: i64,
    estimate: Estimate,
}

fn component(q: Quantity, requests: i64, rate: Option<i64>) -> (PriceComponent, Accumulator) {
    // Zero tokens cost zero at any rate. Keep this observation even when
    // other requests in the same projected bucket have unpriceable tokens.
    let priced = if rate.is_some() { q.known } else { q.zero };
    let total = Accumulator {
        priced: priced > 0,
        partial: priced < requests,
        pico_usd: i128::from(rate.unwrap_or(0)) * q.units,
    };
    (
        PriceComponent {
            tokens: q.field().tokens,
            reported_requests: q.known,
            usd_per_million: rate.map(|r| decimal(i128::from(r), 6)),
            priced_requests: priced,
            estimate: total.estimate(),
        },
        total,
    )
}

fn quote(p: &Price, tally: &ModelTally) -> (ModelPrice, Accumulator) {
    let mut tiers = tally.tiers.clone();
    if p.long.is_none() || p.unsupported.is_some() {
        tiers[0].merge(&tally.tiers[1], 1);
        tiers[0].merge(&tally.tiers[2], 1);
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
    .map(|((tier, tally), rates)| {
        let components = std::array::from_fn(|i| {
            let (component, cost) = component(tally.quantities[i], tally.requests, rates[i]);
            if i != 2 || p.input_billing == InputBilling::SeparateWrite {
                total.merge(cost);
            }
            component
        });
        let baseline_input = (tally.baseline.known > 0).then(|| {
            let (component, cost) = component(tally.baseline, tally.baseline.known, rates[0]);
            total.merge(cost);
            total.partial = true;
            component
        });
        total.partial |= !tally.notes.is_empty();
        PriceTier {
            tier,
            requests: tally.requests,
            components,
            baseline_input,
            notes: tally.notes,
        }
    })
    .collect();
    (
        ModelPrice {
            openrouter_id: p.id,
            canonical_slug: p.canonical_slug,
            snapshot_date: snapshot_date(),
            long_input_above: p
                .long
                .filter(|_| p.unsupported.is_none())
                .map(|_| LONG_INPUT),
            unsupported: p.unsupported,
            input_billing: p.input_billing,
            tiers,
        },
        if p.unsupported.is_some() {
            Accumulator {
                partial: true,
                ..Accumulator::default()
            }
        } else {
            total
        },
    )
}

impl ModelRow {
    fn new(
        model: Option<String>,
        tokens: Totals,
        fresh: Quantity,
        response_model_requests: i64,
        request_level_selected_model_requests: i64,
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
            response_model_requests,
            request_level_selected_model_requests,
            selected_model_requests: model
                .as_ref()
                .map_or(0, |_| tokens.valid_records - response_model_requests),
            estimate_gap: if model.is_none() {
                Some("unknown_model")
            } else if quote.is_none() {
                Some("no_exact_price")
            } else if quote.as_ref().is_some_and(|p| p.unsupported.is_some()) {
                Some("unsupported_price")
            } else if estimate.partial {
                Some("incomplete_components")
            } else {
                None
            },
            model,
            requested_models: BTreeMap::new(),
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
    let mut grouped = BTreeMap::<String, (String, ModelTally, i64)>::new();
    for (entries, confirmed) in [(models.confirmed, true), (models.selected, false)] {
        for (id, tally) in entries {
            // Only the catalog's exact IDs, canonical snapshots and unique
            // short names are equivalent. Do not approximate unlisted IDs.
            let key = price(&id).map_or(id.as_str(), |p| p.id).to_owned();
            let (label, combined, responses) = grouped.entry(key).or_default();
            if label.is_empty() || (id.len(), &id) < (label.len(), label) {
                *label = id;
            }
            combined.merge(&tally, 1);
            if confirmed {
                *responses += tally.tokens.valid_records;
            }
        }
    }
    let mut rows = Vec::with_capacity(grouped.len() + 1);
    let mut unknown = totals;
    let mut unknown_fresh = models.fresh;
    let mut total = Accumulator::default();
    for (id, tally, responses) in grouped.into_values() {
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
            responses,
            tally.request_level_selections,
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
            0,
            0,
            None,
            cost,
        ));
    }
    (rows, total.estimate())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn measure(
        totals: &mut Totals,
        models: &mut ModelProjection,
        id: &str,
        fields: [Option<i64>; 6],
    ) {
        let meter = RequestMeter {
            counts: TokenCounts::from_fields(fields),
            model: Some(TokenModel::Response(id.to_owned())),
            provider_counts: true,
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
        assert_eq!(json!(estimate), json!({"usd":"0.000662","partial":true}));
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

    #[test]
    fn deepseek_prefers_explicit_miss_and_does_not_need_write_or_reasoning() {
        let mut totals = Totals::default();
        let mut models = ModelProjection::default();
        let id = "deepseek-v4.1-flash";
        measure(
            &mut totals,
            &mut models,
            id,
            [Some(120), Some(100), Some(70), None, Some(20), Some(17)],
        );
        for (input, read, write, miss) in [
            (Some(100), Some(70), Some(10), Some(15)),
            (None, None, None, Some(0)),
        ] {
            let meter = RequestMeter {
                counts: TokenCounts {
                    cache_miss_input_tokens: miss,
                    ..TokenCounts::from_fields([None, input, read, write, Some(0), None])
                },
                model: Some(TokenModel::Response(id.into())),
                provider_counts: true,
            };
            totals.count(meter.counts, 1);
            models.count(&meter, 1);
        }
        let (rows, estimate) = rows(models, totals);
        let row = &json!(rows)[0];
        assert_eq!(
            row["cache_write_input_tokens"],
            json!({"tokens":"10", "reported_requests":1})
        );
        assert_eq!(row["reasoning_output_tokens"]["tokens"], "17");
        let tier = &row["price"]["tiers"][0];
        assert_eq!(tier["components"][0]["tokens"], "45");
        assert_eq!(tier["components"][0]["priced_requests"], 3);
        assert_eq!(tier["components"][1]["tokens"], "140");
        assert_eq!(tier["components"][2]["priced_requests"], 0);
        assert_eq!(
            tier["notes"],
            json!({"conflicting_input":1, "unexpected_write":1})
        );
        assert_eq!(json!(estimate), json!({"usd":"0.000015288","partial":true}));
    }

    #[test]
    fn zero_included_and_unknown_write_prices_are_different() {
        let counts = TokenCounts::from_fields([
            Some(120),
            Some(100),
            Some(70),
            Some(10),
            Some(20),
            Some(17),
        ]);
        let mut p = Price {
            id: "test",
            canonical_slug: "test",
            input_billing: InputBilling::SeparateWrite,
            standard: [Some(100), Some(10), Some(0), Some(200)],
            long: None,
            unsupported: None,
        };
        let project = |p: &Price| {
            let mut tally = ModelTally::default();
            tally.count(counts, partition(counts, Some(p.input_billing)), 1);
            let (price, total) = quote(p, &tally);
            (json!(price), json!(total.estimate()))
        };
        let (zero, total) = project(&p);
        assert_eq!(total, json!({"usd":"0.0000000067", "partial":false}));
        assert_eq!(zero["tiers"][0]["components"][2]["usd_per_million"], "0");
        assert_eq!(zero["tiers"][0]["components"][2]["priced_requests"], 1);

        p.standard[2] = None;
        let (unknown, total) = project(&p);
        assert_eq!(total, json!({"usd":"0.0000000067", "partial":true}));
        assert_eq!(
            unknown["tiers"][0]["components"][2]["estimate"]["usd"],
            Value::Null
        );
        assert_eq!(unknown["tiers"][0]["components"][2]["priced_requests"], 0);

        p.input_billing = InputBilling::IncludedWrite;
        let (included, total) = project(&p);
        assert_eq!(total, json!({"usd":"0.0000000077", "partial":false}));
        assert_eq!(included["tiers"][0]["components"][0]["tokens"], "30");
        assert_eq!(included["input_billing"], "included_write");
    }

    #[test]
    fn responses_baseline_deducts_known_write_and_flags_unobserved_read() {
        let mut totals = Totals::default();
        let mut models = ModelProjection::default();
        measure(
            &mut totals,
            &mut models,
            "gpt-6.2-sol",
            [Some(120), Some(100), None, Some(10), Some(20), Some(17)],
        );
        measure(
            &mut totals,
            &mut models,
            "gpt-5.4-mini",
            [Some(120), Some(100), Some(70), None, Some(20), None],
        );
        let (rows, _) = rows(models, totals);
        let rows = json!(rows);
        assert_eq!(
            rows[0]["estimate"],
            json!({"usd":"0.00011775","partial":false})
        );
        let separate = &rows[1];
        assert_eq!(separate["cached_input_tokens"]["tokens"], Value::Null);
        assert_eq!(separate["uncached_input_tokens"]["tokens"], Value::Null);
        let tier = &separate["price"]["tiers"][0];
        assert_eq!(tier["baseline_input"]["tokens"], "90");
        assert_eq!(tier["notes"], json!({"read_discount_unobserved":1}));
        assert_eq!(
            separate["estimate"],
            json!({"usd":"0.000405","partial":true})
        );
    }
}
