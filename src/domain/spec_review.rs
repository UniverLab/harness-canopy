use serde::{Deserialize, Serialize};

/// The built-in adversarial spec review's verdict state for a spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpecReviewState {
    #[default]
    Unreviewed,
    InReview,
    Approved,
    Rejected,
    ReviewError,
}

impl SpecReviewState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unreviewed => "unreviewed",
            Self::InReview => "in_review",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::ReviewError => "review_error",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "in_review" => Self::InReview,
            "approved" => Self::Approved,
            "rejected" => Self::Rejected,
            "review_error" => Self::ReviewError,
            _ => Self::Unreviewed,
        }
    }

    pub fn is_runnable(self) -> bool {
        matches!(self, Self::Approved)
    }
}

/// Classification the reviewer assigns to an approved spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecReviewTier {
    Trivial,
    Normal,
}

impl SpecReviewTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trivial => "trivial",
            Self::Normal => "normal",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "trivial" => Some(Self::Trivial),
            "normal" => Some(Self::Normal),
            _ => None,
        }
    }
}

/// One finding reported by the spec reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecReviewFinding {
    pub severity: String,
    pub text: String,
}
