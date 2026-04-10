#![allow(dead_code)]
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sector {
    Ai,
    Miami,
    Italy,
    Tech,
}

impl Sector {
    pub fn as_str(&self) -> &'static str {
        match self {
            Sector::Ai => "ai",
            Sector::Miami => "miami",
            Sector::Italy => "italy",
            Sector::Tech => "tech",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Sector::Ai => "AI & LLMs",
            Sector::Miami => "Miami Beach",
            Sector::Italy => "Italian Politics",
            Sector::Tech => "Tech & Innovation",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "ai" => Some(Sector::Ai),
            "miami" => Some(Sector::Miami),
            "italy" => Some(Sector::Italy),
            "tech" => Some(Sector::Tech),
            _ => None,
        }
    }
}
