use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum IdentityGeneration {
    Always,
    ByDefault,
}

impl IdentityGeneration {
    #[must_use]
    pub fn as_sql(self) -> &'static str {
        match self {
            Self::Always => "ALWAYS",
            Self::ByDefault => "BY DEFAULT",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct IdentityOptions {
    pub start_value: Option<i64>,
    pub increment_by: Option<i64>,
    pub min_value: Option<i64>,
    pub max_value: Option<i64>,
    pub cycle: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IdentitySpec {
    pub generation: IdentityGeneration,
    pub options: IdentityOptions,
}
