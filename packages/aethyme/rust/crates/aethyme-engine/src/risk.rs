#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum RiskArea {
    Auth,
    Permissions,
    Secrets,
    Migrations,
    Infra,
    Billing,
    SharedCore,
    Destructive,
    UserDefined(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct RiskFlag {
    pub scope: String,
    pub area: RiskArea,
    pub level: RiskLevel,
    pub reason: String,
}

impl RiskFlag {
    pub fn new(
        scope: impl Into<String>,
        area: RiskArea,
        level: RiskLevel,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            scope: scope.into(),
            area,
            level,
            reason: reason.into(),
        }
    }
}
