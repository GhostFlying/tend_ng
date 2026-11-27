use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenCounts {
    pub total_tokens: Option<i64>,
    pub input_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub cache_write_input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_output_tokens: Option<i64>,
}

impl TokenCounts {
    pub fn fields(self) -> [Option<i64>; 6] {
        [
            self.total_tokens,
            self.input_tokens,
            self.cached_input_tokens,
            self.cache_write_input_tokens,
            self.output_tokens,
            self.reasoning_output_tokens,
        ]
    }

    pub fn from_fields(fields: [Option<i64>; 6]) -> Self {
        let [
            total_tokens,
            input_tokens,
            cached_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens,
        ] = fields;
        Self {
            total_tokens,
            input_tokens,
            cached_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens,
        }
    }

    pub fn merge_known(self, other: Self) -> Option<Self> {
        let mut fields = self.fields();
        for (left, right) in fields.iter_mut().zip(other.fields()) {
            if left.is_some() && right.is_some() && *left != right {
                return None;
            }
            *left = left.or(right);
        }
        Some(Self::from_fields(fields))
    }

    /// Exact provider usage takes precedence over compatible normalized
    /// copies. A runtime default must not fill an omitted provider field.
    pub fn merge_reported(self, self_exact: bool, other: Self, other_exact: bool) -> Option<Self> {
        let merged = self.merge_known(other)?;
        Some(match (self_exact, other_exact) {
            (true, false) => self,
            (false, true) => other,
            _ => merged,
        })
    }

    /// Legacy Codex fills omitted optional provider details with zero.
    pub fn without_unproven_optional_zeros(mut self) -> Self {
        for detail in [
            &mut self.cached_input_tokens,
            &mut self.cache_write_input_tokens,
            &mut self.reasoning_output_tokens,
        ] {
            if *detail == Some(0) {
                *detail = None;
            }
        }
        self
    }

    pub fn missing(self) -> bool {
        self.fields().iter().all(Option::is_none)
    }

    pub fn valid(self) -> bool {
        let fields = self.fields();
        fields.iter().any(Option::is_some) && fields.into_iter().flatten().all(|n| n >= 0)
    }
}

/// Model evidence from the native rollout, not from the assigned Agent.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(from = "ModelWire", into = "ModelWire")]
pub enum TokenModel {
    /// Native actual model associated with the exact response.
    Response(String),
    /// Model selected at execution time, from the native turn.
    TurnContext(String),
    /// Model selected at execution time and bound to the specific request.
    RequestSelection(String),
    /// A normalized request was checked without finding model evidence.
    /// Absent (None) means a legacy writer did not perform this check.
    Unresolved,
    Ambiguous,
    TurnConflict,
    RequestSelectionConflict,
    LegacyResponseConflict,
    ResponseConflict,
    /// An exact request carries a reason why an old reader lost attribution.
    Rechecked(Box<Self>, ModelRepair),
    /// An older peer reported uncertainty. Keep independent observations
    /// even when that uncertainty temporarily masks the displayed model.
    PendingLegacy(Box<Self>, LegacyModelMarkers),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct LegacyModelMarkers {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    turn: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    response: bool,
}

impl LegacyModelMarkers {
    fn empty(&self) -> bool {
        *self == Self::default()
    }

    fn combine(self, other: Self) -> Self {
        Self {
            turn: self.turn || other.turn,
            response: self.response || other.response,
        }
    }

    fn repair(self, proof: ModelRepair) -> Self {
        Self {
            turn: self.turn && !(proof.unkeyed_reroute || proof.equivalent_turn_ids),
            response: self.response && !(proof.malformed_response || proof.equivalent_response_ids),
        }
    }

    fn visible(self, observed: &TokenModel) -> &TokenModel {
        if self.response && !matches!(observed, TokenModel::ResponseConflict) {
            &TokenModel::LegacyResponseConflict
        } else if self.turn
            && matches!(
                observed,
                TokenModel::Unresolved | TokenModel::TurnContext(_)
            )
        {
            &TokenModel::Ambiguous
        } else {
            observed
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ModelRepair {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    unkeyed_reroute: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    malformed_response: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    equivalent_response_ids: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    equivalent_turn_ids: bool,
}

impl ModelRepair {
    pub fn native(unkeyed_reroute: bool, malformed_response: bool) -> Self {
        Self {
            unkeyed_reroute,
            malformed_response,
            ..Self::default()
        }
    }

    fn empty(&self) -> bool {
        *self == Self::default()
    }

    fn combine(self, other: Self) -> Self {
        Self {
            unkeyed_reroute: self.unkeyed_reroute || other.unkeyed_reroute,
            malformed_response: self.malformed_response || other.malformed_response,
            equivalent_response_ids: self.equivalent_response_ids || other.equivalent_response_ids,
            equivalent_turn_ids: self.equivalent_turn_ids || other.equivalent_turn_ids,
        }
    }
}

#[derive(Deserialize, Serialize)]
struct ModelWire {
    #[serde(flatten)]
    visible: ModelKind,
    #[serde(default, skip_serializing_if = "ModelRepair::empty")]
    rechecked: ModelRepair,
    // Unknown remains unknown for older peers. New peers use the held
    // claim when independent native evidence clears the legacy marker.
    #[serde(default, skip_serializing_if = "LegacyModelMarkers::empty")]
    pending: LegacyModelMarkers,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    held_model: Option<ModelKind>,
}

#[derive(Deserialize, Serialize)]
struct ModelKind {
    #[serde(flatten)]
    value: ModelValue,
    // Older peers still recognize `turn_context` and `ambiguous`. They
    // retain the request counts even if they cannot rank this evidence.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    request_level: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    observed_conflict: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "source", content = "id")]
enum ModelValue {
    Response(String),
    TurnContext(String),
    Unresolved,
    Ambiguous,
    ResponseConflict,
}

impl From<ModelWire> for TokenModel {
    fn from(wire: ModelWire) -> Self {
        let visible = Self::from(wire.visible);
        let (observed, _, implicit) = visible.split();
        let (observed, held) = wire.held_model.filter(|_| !implicit.empty()).map_or(
            (observed, LegacyModelMarkers::default()),
            |held| {
                let (observed, _, pending) = Self::from(held).split();
                (observed, pending)
            },
        );
        Self::compose(
            observed,
            wire.rechecked,
            wire.pending.combine(implicit).combine(held),
        )
    }
}

impl From<ModelKind> for TokenModel {
    fn from(kind: ModelKind) -> Self {
        match kind.value {
            ModelValue::Response(id) => Self::Response(id),
            ModelValue::TurnContext(id) if kind.request_level => Self::RequestSelection(id),
            ModelValue::TurnContext(id) => Self::TurnContext(id),
            ModelValue::Unresolved => Self::Unresolved,
            ModelValue::Ambiguous if kind.request_level => Self::RequestSelectionConflict,
            ModelValue::Ambiguous if kind.observed_conflict => Self::TurnConflict,
            ModelValue::Ambiguous => Self::Ambiguous,
            ModelValue::ResponseConflict if kind.observed_conflict => Self::ResponseConflict,
            ModelValue::ResponseConflict => Self::LegacyResponseConflict,
        }
    }
}

impl From<TokenModel> for ModelWire {
    fn from(model: TokenModel) -> Self {
        let visible = model.basis().clone();
        let (observed, rechecked, mut pending) = model.split();
        if matches!(visible, TokenModel::Ambiguous) {
            pending.turn = false;
        }
        if matches!(visible, TokenModel::LegacyResponseConflict) {
            pending.response = false;
        }
        Self {
            held_model: (observed != visible && observed != TokenModel::Unresolved)
                .then(|| ModelKind::from(observed)),
            visible: ModelKind::from(visible),
            rechecked,
            pending,
        }
    }
}

impl From<TokenModel> for ModelKind {
    fn from(model: TokenModel) -> Self {
        let (value, request_level, observed_conflict) = match model {
            TokenModel::Response(id) => (ModelValue::Response(id), false, false),
            TokenModel::TurnContext(id) => (ModelValue::TurnContext(id), false, false),
            TokenModel::RequestSelection(id) => (ModelValue::TurnContext(id), true, false),
            TokenModel::Unresolved => (ModelValue::Unresolved, false, false),
            TokenModel::Ambiguous => (ModelValue::Ambiguous, false, false),
            TokenModel::TurnConflict => (ModelValue::Ambiguous, false, true),
            TokenModel::RequestSelectionConflict => (ModelValue::Ambiguous, true, false),
            TokenModel::LegacyResponseConflict => (ModelValue::ResponseConflict, false, false),
            TokenModel::ResponseConflict => (ModelValue::ResponseConflict, false, true),
            TokenModel::Rechecked(_, _) | TokenModel::PendingLegacy(_, _) => unreachable!(),
        };
        Self {
            value,
            request_level,
            observed_conflict,
        }
    }
}

impl TokenModel {
    pub fn valid_id(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b':'))
    }

    pub fn valid(&self) -> bool {
        match self {
            Self::Response(id) | Self::TurnContext(id) | Self::RequestSelection(id) => {
                Self::valid_id(id)
            }
            Self::Unresolved
            | Self::Ambiguous
            | Self::TurnConflict
            | Self::RequestSelectionConflict
            | Self::LegacyResponseConflict
            | Self::ResponseConflict => true,
            Self::Rechecked(model, _) | Self::PendingLegacy(model, _) => model.valid(),
        }
    }

    pub fn basis(&self) -> &Self {
        match self {
            Self::Rechecked(model, _) => model.basis(),
            Self::PendingLegacy(model, pending) => pending.visible(model.basis()),
            model => model,
        }
    }

    fn split(self) -> (Self, ModelRepair, LegacyModelMarkers) {
        match self {
            Self::Rechecked(model, repair) => {
                let (model, inner, pending) = model.split();
                (model, repair.combine(inner), pending)
            }
            Self::PendingLegacy(model, pending) => {
                let (model, repair, inner) = model.split();
                (model, repair, pending.combine(inner))
            }
            Self::Ambiguous => (
                Self::Unresolved,
                ModelRepair::default(),
                LegacyModelMarkers {
                    turn: true,
                    response: false,
                },
            ),
            Self::LegacyResponseConflict => (
                Self::Unresolved,
                ModelRepair::default(),
                LegacyModelMarkers {
                    turn: false,
                    response: true,
                },
            ),
            model => (model, ModelRepair::default(), LegacyModelMarkers::default()),
        }
    }

    pub fn rechecked(self, repair: ModelRepair) -> Self {
        let (model, old, pending) = self.split();
        Self::compose(model, repair.combine(old), pending)
    }

    fn compose(observed: Self, repair: ModelRepair, pending: LegacyModelMarkers) -> Self {
        let pending = pending.repair(repair);
        let model = match (pending.turn, pending.response, observed) {
            (false, false, model) => model,
            (true, false, Self::Unresolved) => Self::Ambiguous,
            (false, true, Self::Unresolved) => Self::LegacyResponseConflict,
            (_, _, model) => Self::PendingLegacy(Box::new(model), pending),
        };
        if repair.empty() {
            model
        } else {
            Self::Rechecked(Box::new(model), repair)
        }
    }

    /// Remove new metadata from the bytes of a pre-child reader's snapshot.
    fn legacy(self) -> Self {
        match self.basis().clone() {
            Self::TurnConflict => Self::Ambiguous,
            Self::ResponseConflict => Self::LegacyResponseConflict,
            model => model,
        }
    }

    /// Pre-child readers retain their original representation and exact
    /// raw-string comparisons so a previously issued page can be replayed.
    pub fn merge_legacy(old: Option<Self>, new: Option<Self>) -> Option<Self> {
        Self::merge_basis(old.map(Self::legacy), new.map(Self::legacy)).map(Self::legacy)
    }

    fn selected_id(&self) -> Option<(u8, &str)> {
        match self {
            Self::Response(id) => Some((3, id)),
            Self::RequestSelection(id) => Some((2, id)),
            Self::TurnContext(id) => Some((1, id)),
            _ => None,
        }
    }

    /// No evidence cannot revoke existing evidence. A contradiction at the
    /// same evidence level must not silently choose a new model.
    pub fn merge(old: Option<Self>, new: Option<Self>) -> Option<Self> {
        if old.is_none() && new.is_none() {
            return None;
        }
        let (old, old_repair, old_pending) = old.unwrap_or(Self::Unresolved).split();
        let (new, new_repair, new_pending) = new.unwrap_or(Self::Unresolved).split();
        let mut repair = old_repair.combine(new_repair);
        let pending = old_pending.combine(new_pending);
        let old_id = old.selected_id();
        let new_id = new.selected_id();
        if let (Some((rank, a)), Some((other_rank, b))) = (old_id, new_id)
            && rank == other_rank
            && a != b
            && crate::token_prices::price(a)
                .zip(crate::token_prices::price(b))
                .is_some_and(|(a, b)| a.id == b.id)
        {
            // Which spelling was seen first has no bearing on identity.
            repair.equivalent_response_ids |= rank == 3;
            repair.equivalent_turn_ids |= rank == 1;
            let model = if (a.len(), a) <= (b.len(), b) {
                old
            } else {
                new
            };
            return Some(Self::compose(model, repair, pending));
        }
        // Combine evidence first, then apply legacy uncertainty only to its
        // visible projection. Later rescans must still see earlier claims.
        Self::merge_basis(Some(old), Some(new)).map(|model| Self::compose(model, repair, pending))
    }

    fn merge_basis(old: Option<Self>, new: Option<Self>) -> Option<Self> {
        match (old, new) {
            (None, m) | (m, None) => m,
            (Some(Self::Unresolved), m) | (m, Some(Self::Unresolved)) => m,
            (Some(a), Some(b)) if a == b => Some(a),
            (Some(Self::ResponseConflict), _)
            | (_, Some(Self::ResponseConflict))
            | (Some(Self::Response(_)), Some(Self::Response(_))) => Some(Self::ResponseConflict),
            (Some(Self::LegacyResponseConflict), _) | (_, Some(Self::LegacyResponseConflict)) => {
                Some(Self::LegacyResponseConflict)
            }
            (Some(m @ Self::Response(_)), _) | (_, Some(m @ Self::Response(_))) => Some(m),
            (Some(Self::RequestSelectionConflict), _)
            | (_, Some(Self::RequestSelectionConflict))
            | (Some(Self::RequestSelection(_)), Some(Self::RequestSelection(_))) => {
                Some(Self::RequestSelectionConflict)
            }
            (Some(m @ Self::RequestSelection(_)), _) | (_, Some(m @ Self::RequestSelection(_))) => {
                Some(m)
            }
            (Some(Self::TurnConflict), _)
            | (_, Some(Self::TurnConflict))
            | (Some(Self::TurnContext(_)), Some(Self::TurnContext(_))) => Some(Self::TurnConflict),
            _ => Some(Self::Ambiguous),
        }
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;

    #[test]
    fn exact_selection_outranks_turn_context() {
        let context = TokenModel::TurnContext("context".into());
        let exact = TokenModel::RequestSelection("selected".into());
        let actual = TokenModel::Response("rerouted".into());
        assert_eq!(
            TokenModel::merge(Some(exact.clone()), Some(context)),
            Some(exact.clone())
        );
        assert_eq!(
            TokenModel::merge(Some(exact.clone()), Some(actual.clone())),
            Some(actual)
        );
        assert_eq!(
            TokenModel::merge(
                Some(exact),
                Some(TokenModel::RequestSelection("other".into()))
            ),
            Some(TokenModel::RequestSelectionConflict)
        );
    }

    #[test]
    fn native_recheck_only_removes_the_explained_legacy_marker() {
        let selected = TokenModel::TurnContext("selected".into());
        let reroute = selected.clone().rechecked(ModelRepair {
            unkeyed_reroute: true,
            ..Default::default()
        });
        let malformed = selected.rechecked(ModelRepair {
            malformed_response: true,
            ..Default::default()
        });
        for (marker, repair) in [
            (TokenModel::Ambiguous, reroute.clone()),
            (TokenModel::LegacyResponseConflict, malformed.clone()),
        ] {
            assert_eq!(
                TokenModel::merge(Some(marker.clone()), Some(repair.clone())),
                Some(repair.clone())
            );
            assert_eq!(
                TokenModel::merge(Some(repair.clone()), Some(marker)),
                Some(repair)
            );
        }
        assert_eq!(
            TokenModel::merge(Some(reroute.clone()), Some(TokenModel::TurnConflict))
                .as_ref()
                .map(TokenModel::basis),
            Some(&TokenModel::TurnConflict)
        );
        assert_eq!(
            TokenModel::merge(Some(malformed), Some(TokenModel::ResponseConflict))
                .as_ref()
                .map(TokenModel::basis),
            Some(&TokenModel::ResponseConflict)
        );
        assert_eq!(
            TokenModel::merge(Some(reroute), Some(TokenModel::LegacyResponseConflict))
                .as_ref()
                .map(TokenModel::basis),
            Some(&TokenModel::LegacyResponseConflict)
        );
    }

    #[test]
    fn exact_catalog_aliases_resolve_at_every_evidence_rank() {
        for model in [
            TokenModel::TurnContext as fn(String) -> TokenModel,
            TokenModel::RequestSelection,
            TokenModel::Response,
        ] {
            let short = model("gpt-6.2-sol".into());
            let full = model("openai/gpt-6.2-sol-20261117".into());
            let unlisted = model("gpt-6.2-sol:batch".into());
            let forward = TokenModel::merge(Some(full.clone()), Some(short.clone())).unwrap();
            let reverse = TokenModel::merge(Some(short.clone()), Some(full)).unwrap();
            assert_eq!(forward, reverse);
            assert_eq!(forward.basis(), &short);
            assert!(
                TokenModel::merge(Some(short), Some(unlisted))
                    .unwrap()
                    .selected_id()
                    .is_none()
            );
        }
    }

    #[test]
    fn legacy_uncertainty_never_discards_later_actual_evidence() {
        let legacy = TokenModel::LegacyResponseConflict;
        let live = TokenModel::Response("gpt-6.2-sol".into());
        let native = TokenModel::Response("gpt-6.2-astra".into())
            .rechecked(ModelRepair::native(false, true));
        let held = TokenModel::merge(Some(legacy.clone()), Some(live.clone())).unwrap();
        assert_eq!(held.basis(), &legacy);
        assert_eq!(
            serde_json::to_value(&held).unwrap(),
            serde_json::json!({"source":"response_conflict",
                "held_model":{"source":"response","id":"gpt-6.2-sol"}})
        );
        assert_eq!(
            TokenModel::merge(
                Some(held.clone()),
                Some(live.rechecked(ModelRepair::native(false, true)))
            )
            .unwrap()
            .basis(),
            &TokenModel::Response("gpt-6.2-sol".into())
        );
        let evidence = [legacy, TokenModel::Response("gpt-6.2-sol".into()), native];
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let evidence = order
                .into_iter()
                .fold(None, |merged, index| {
                    let next = TokenModel::merge(merged, Some(evidence[index].clone())).unwrap();
                    let encoded = serde_json::to_string(&next).unwrap();
                    let decoded = serde_json::from_str(&encoded).unwrap();
                    assert_eq!(next, decoded);
                    Some(decoded)
                })
                .unwrap();
            assert_eq!(evidence.basis(), &TokenModel::ResponseConflict);
        }
    }
}

/// Request evidence stored alongside the original flat counts JSON. This
/// reads legacy records and preserves the count payload on older peers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestMeter {
    #[serde(flatten)]
    pub counts: TokenCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TokenModel>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provider_counts: bool,
}

impl RequestMeter {
    pub fn valid(&self) -> bool {
        Self::valid_evidence(self.counts, self.model.as_ref())
    }

    fn valid_evidence(counts: TokenCounts, model: Option<&TokenModel>) -> bool {
        (counts.valid() || (counts.missing() && model.is_some()))
            && model.is_none_or(TokenModel::valid)
    }

    /// Apply only after corroborating the response identity. An older wire
    /// observation can contain only optional defaults and no model evidence.
    pub fn normalized(mut self) -> Self {
        if !self.provider_counts {
            self.counts = self.counts.without_unproven_optional_zeros();
        }
        if self.counts.missing() && self.model.is_none() {
            self.model = Some(TokenModel::Unresolved);
        }
        self
    }

    pub fn merge(&self, other: &Self) -> Option<Self> {
        Some(Self {
            counts: self.counts.merge_reported(
                self.provider_counts,
                other.counts,
                other.provider_counts,
            )?,
            model: TokenModel::merge(self.model.clone(), other.model.clone()),
            provider_counts: self.provider_counts || other.provider_counts,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NativeTurnKey {
    pub thread_id: String,
    pub turn_id: String,
}

impl NativeTurnKey {
    pub fn valid(&self) -> bool {
        [&self.thread_id, &self.turn_id]
            .into_iter()
            .all(|id| valid_native_id(id))
    }
}

pub fn valid_native_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

pub fn native_request_cursor(child: Option<(&str, &str)>, response_id: &str) -> String {
    match child {
        None => response_id.to_owned(),
        Some((thread, turn)) => format!("\x1f{thread}\x1f{turn}\x1f{response_id}"),
    }
}

/// A native request observation with a corroborated upstream response ID.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunTokenUsage {
    pub event_id: String,
    pub assignment_id: String,
    pub task_id: String,
    pub run_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub response_id: String,
    /// The accepted Run lives on this causal root. The other thread and
    /// turn are the request's original native identity, not aliases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<NativeTurnKey>,
    pub last: TokenCounts,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provider_counts: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TokenModel>,
    pub observed_at: i64,
}

impl RunTokenUsage {
    pub fn valid(&self) -> bool {
        [
            &self.event_id,
            &self.assignment_id,
            &self.task_id,
            &self.run_id,
        ]
        .into_iter()
        .all(|id| crate::valid_id(id))
            && [&self.thread_id, &self.turn_id, &self.response_id]
                .into_iter()
                .all(|id| valid_native_id(id))
            && self.root.as_ref().is_none_or(|root| {
                root.valid() && root.thread_id != self.thread_id && root.turn_id != self.turn_id
            })
            && RequestMeter::valid_evidence(self.last, self.model.as_ref())
            && self.observed_at > 0
    }
}

pub const TOKEN_OVERVIEW_PATH: &str = "/api/v1/token-usage";
pub const TOKEN_BACKFILL_PATH: &str = "/api/v1/token-usage/backfills";
pub const TOKEN_REBUILD_PATH: &str = "/api/v1/token-usage/rebuild";

#[derive(Debug, Eq, PartialEq)]
pub struct TokenOverviewCursor {
    pub source_seq: i64,
    pub issue_id: String,
}

impl TokenOverviewCursor {
    pub fn encode(&self) -> String {
        format!("v1.{}.{}", self.source_seq, self.issue_id)
    }

    pub fn decode(cursor: &str) -> Option<Self> {
        let (seq, issue_id) = cursor.strip_prefix("v1.")?.split_once('.')?;
        let source_seq = seq.parse::<i64>().ok()?;
        (seq == source_seq.to_string() && crate::valid_id(issue_id)).then(|| Self {
            source_seq,
            issue_id: issue_id.to_owned(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum TokenUsageScope {
    All,
    Run(String),
    Task(String),
    Issue(String),
    Runner(String),
    Host(String),
}

impl TokenUsageScope {
    pub fn valid(&self) -> bool {
        match self {
            Self::All => true,
            Self::Run(id) | Self::Task(id) | Self::Runner(id) | Self::Host(id) => {
                crate::valid_id(id)
            }
            Self::Issue(id) => crate::valid_id(id) || crate::parse_issue_number(id).is_some(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenConflictPolicy {
    #[default]
    Quarantine,
    PreferNative,
    PreferServer,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StartTokenBackfill {
    pub job_id: String,
    pub scope: TokenUsageScope,
    #[serde(default)]
    pub policy: TokenConflictPolicy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenBackfillWork {
    pub item: i64,
    pub job_id: String,
    pub runner_id: String,
    pub run_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub offset: i64,
    /// A new Server explicitly opts in before a Runner may send child keys.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_children: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenSourceGap {
    BindingMissing,
    SourceMissing,
    SourceAmbiguous,
    SourceUnreadable,
    SourceCorrupt,
    OversizedNativeRecord,
    SourceChanged,
    NoNativeRecords,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeTokenRecord {
    pub response_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<NativeTurnKey>,
    pub counts: Option<TokenCounts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TokenModel>,
    pub invalid: bool,
    /// A second distinct variant proves that the native key is ambiguous.
    pub variant: Option<TokenCounts>,
}

impl NativeTokenRecord {
    pub fn cursor(&self) -> String {
        native_request_cursor(
            self.child
                .as_ref()
                .map(|key| (key.thread_id.as_str(), key.turn_id.as_str())),
            &self.response_id,
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TokenBackfillPage {
    pub item: i64,
    pub offset: i64,
    #[serde(flatten)]
    pub content: TokenBackfillContent,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TokenBackfillContent {
    Gap {
        reason: TokenSourceGap,
    },
    Records {
        snapshot: String,
        records: Vec<NativeTokenRecord>,
        done: bool,
        prefix: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child_gap: Option<TokenSourceGap>,
        /// Absent on older root-only readers. True attests that child
        /// discovery was attempted for this snapshot, even if it has gaps.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        child_checked: bool,
    },
}

/// Opt-in on Runner-wide sync only. Presence advertises support even with no page.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct TokenBackfillSync {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<TokenBackfillPage>,
}

impl TokenBackfillPage {
    pub fn valid(&self) -> bool {
        if self.item <= 0 || self.offset < 0 {
            return false;
        }
        match &self.content {
            TokenBackfillContent::Gap { .. } => true,
            TokenBackfillContent::Records {
                snapshot,
                records,
                done,
                ..
            } => {
                snapshot.len() == 64
                    && snapshot.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && !records.is_empty()
                    && records.len() <= crate::RUNNER_SYNC_BATCH
                    && (*done || records.len() == crate::RUNNER_SYNC_BATCH)
                    && records.iter().all(|record| {
                        valid_native_id(&record.response_id)
                            && record.child.as_ref().is_none_or(NativeTurnKey::valid)
                            && record.model.as_ref().is_none_or(TokenModel::valid)
                    })
                    && records
                        .windows(2)
                        .all(|rows| rows[0].cursor() < rows[1].cursor())
            }
        }
    }
}
