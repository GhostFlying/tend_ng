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

    pub fn valid(self) -> bool {
        let fields = self.fields();
        fields.iter().any(Option::is_some) && fields.into_iter().flatten().all(|n| n >= 0)
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
    pub last: TokenCounts,
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
                .all(|id| !id.is_empty() && id.len() <= 256)
            && self.last.valid()
            && self.observed_at > 0
    }
}
