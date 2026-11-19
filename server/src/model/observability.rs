//! Defines provider call and usage observability records.

mod usage {
    use std::ops::AddAssign;

    use serde::{Deserialize, Serialize};

    #[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
    pub struct Usage {
        pub input_tokens: Option<u64>,
        pub context_input_tokens: Option<u64>,
        pub output_tokens: Option<u64>,
        pub total_tokens: Option<u64>,
        pub cache_read_tokens: Option<u64>,
        pub cache_write_tokens: Option<u64>,
        pub reasoning_tokens: Option<u64>,
    }

    impl AddAssign for Usage {
        fn add_assign(&mut self, rhs: Self) {
            self.input_tokens = sum(self.input_tokens, rhs.input_tokens);
            self.context_input_tokens = sum(self.context_input_tokens, rhs.context_input_tokens);
            self.output_tokens = sum(self.output_tokens, rhs.output_tokens);
            self.total_tokens = sum(self.total_tokens, rhs.total_tokens);
            self.cache_read_tokens = sum(self.cache_read_tokens, rhs.cache_read_tokens);
            self.cache_write_tokens = sum(self.cache_write_tokens, rhs.cache_write_tokens);
            self.reasoning_tokens = sum(self.reasoning_tokens, rhs.reasoning_tokens);
        }
    }

    /// Adds two optional counts, treating an unreported count as zero.
    /// The result is only unknown when neither side reported the field.
    fn sum(left: Option<u64>, right: Option<u64>) -> Option<u64> {
        if left.is_none() && right.is_none() {
            return None;
        }
        Some(
            left.unwrap_or_default()
                .saturating_add(right.unwrap_or_default()),
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_record_without_cache_details_keeps_the_counts_already_accumulated() {
            let mut total = Usage {
                input_tokens: Some(1_000),
                output_tokens: Some(20),
                total_tokens: Some(1_020),
                cache_read_tokens: Some(900),
                ..Default::default()
            };
            // A second provider call that omits `prompt_tokens_details`.
            total += Usage {
                input_tokens: Some(1_200),
                output_tokens: Some(30),
                total_tokens: Some(1_230),
                ..Default::default()
            };
            assert_eq!(total.input_tokens, Some(2_200));
            assert_eq!(total.output_tokens, Some(50));
            assert_eq!(total.total_tokens, Some(2_250));
            assert_eq!(total.cache_read_tokens, Some(900));
        }

        #[test]
        fn a_late_reported_count_is_not_discarded() {
            let mut total = Usage {
                input_tokens: Some(1_000),
                ..Default::default()
            };
            total += Usage {
                input_tokens: Some(1_200),
                cache_read_tokens: Some(900),
                reasoning_tokens: Some(64),
                ..Default::default()
            };
            assert_eq!(total.cache_read_tokens, Some(900));
            assert_eq!(total.reasoning_tokens, Some(64));
        }

        #[test]
        fn a_field_no_call_reported_stays_unknown() {
            let mut total = Usage {
                input_tokens: Some(1_000),
                ..Default::default()
            };
            total += Usage {
                input_tokens: Some(1_200),
                ..Default::default()
            };
            assert_eq!(total.cache_write_tokens, None);
        }
    }
}
pub use usage::*;
