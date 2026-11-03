//! Background-animation budget for the TUI (`[tui] animations`).

use serde::{Deserialize, Serialize};

/// `full` keeps the 20 fps tick, `reduced` (the default) throttles background
/// animations to 8 fps and pauses them on focus loss or 45 s idle, and `off`
/// draws a static frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnimationsMode {
    Full,
    #[default]
    Reduced,
    Off,
}

impl AnimationsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Reduced => "reduced",
            Self::Off => "off",
        }
    }
}

impl std::str::FromStr for AnimationsMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "full" => Ok(Self::Full),
            "reduced" => Ok(Self::Reduced),
            "off" => Ok(Self::Off),
            other => Err(format!("invalid animations mode: {other}")),
        }
    }
}
