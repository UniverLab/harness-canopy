//! Settings for the built-in adversarial spec review, read from the
//! `[spec_review]` table in `config.toml`. Off by default: while `enabled` is
//! false nothing about spec creation changes and `intent` stays optional.

use serde::{Deserialize, Serialize};

use crate::domain::settings;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpecReviewConfig {
    #[serde(default = "default_spec_review_enabled")]
    pub enabled: bool,
    #[serde(default = "default_spec_review_platform")]
    pub platform: String,
    #[serde(default = "default_spec_review_model")]
    pub model: String,
    #[serde(default = "default_spec_review_max_parallel")]
    pub max_parallel: u32,
}

impl Default for SpecReviewConfig {
    fn default() -> Self {
        Self {
            enabled: default_spec_review_enabled(),
            platform: default_spec_review_platform(),
            model: default_spec_review_model(),
            max_parallel: default_spec_review_max_parallel(),
        }
    }
}

fn default_spec_review_enabled() -> bool {
    settings::default_bool("spec_review.enabled")
}

fn default_spec_review_platform() -> String {
    settings::default_string("spec_review.platform")
}

fn default_spec_review_model() -> String {
    settings::default_string("spec_review.model")
}

fn default_spec_review_max_parallel() -> u32 {
    settings::default_int("spec_review.max_parallel") as u32
}
