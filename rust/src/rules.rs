//! Rule definitions and scopes: one provider-style limit per `LimitRule`, keyed
//! per scope so the same rule can isolate a fleet, a tenant, or a user.

use std::fmt;
use std::str::FromStr;

/// Where a limit is enforced. A request is keyed differently per scope so the
/// same rule isolates one global fleet, one tenant, or one user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// A single limit shared across the whole fleet.
    Global,
    /// A limit enforced per tenant.
    Tenant,
    /// A limit enforced per user within a tenant.
    User,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Scope::Global => "global",
            Scope::Tenant => "tenant",
            Scope::User => "user",
        };
        f.write_str(s)
    }
}

impl FromStr for Scope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "global" => Ok(Scope::Global),
            "tenant" => Ok(Scope::Tenant),
            "user" => Ok(Scope::User),
            other => Err(format!("unknown scope \"{other}\"")),
        }
    }
}

/// One provider-style limit. Any subset of the `max_*` dimensions may be set; a
/// request must stay under every one that is.
#[derive(Debug, Clone)]
pub struct LimitRule {
    /// The model this rule applies to; `"*"` matches every model.
    pub model: String,
    /// The rolling window width in seconds.
    pub window_seconds: f64,
    /// Optional cap on token volume over the window.
    pub max_tokens: Option<f64>,
    /// Optional cap on request count over the window.
    pub max_requests: Option<f64>,
    /// Optional cap on dollar spend over the window.
    pub max_cost: Option<f64>,
    /// Optional cap on the number of in-flight requests.
    pub max_concurrent: Option<i64>,
    /// The scope the rule is enforced at.
    pub scope: Scope,
    /// The number of buckets the window is split into (memory/accuracy trade-off).
    pub buckets_per_window: i64,
    /// When true, keep an exact per-event log instead of buckets.
    pub precise: bool,
    /// Optional human-readable label.
    pub name: Option<String>,
}

impl LimitRule {
    /// Builds a rule with the standard defaults (Global scope, 60 buckets).
    pub fn new(model: &str, window_seconds: f64) -> Self {
        LimitRule {
            model: model.to_string(),
            window_seconds,
            max_tokens: None,
            max_requests: None,
            max_cost: None,
            max_concurrent: None,
            scope: Scope::Global,
            buckets_per_window: 60,
            precise: false,
            name: None,
        }
    }

    /// Sets the token cap.
    pub fn max_tokens(mut self, v: f64) -> Self {
        self.max_tokens = Some(v);
        self
    }

    /// Sets the request cap.
    pub fn max_requests(mut self, v: f64) -> Self {
        self.max_requests = Some(v);
        self
    }

    /// Sets the dollar-spend cap.
    pub fn max_cost(mut self, v: f64) -> Self {
        self.max_cost = Some(v);
        self
    }

    /// Sets the in-flight concurrency cap.
    pub fn max_concurrent(mut self, v: i64) -> Self {
        self.max_concurrent = Some(v);
        self
    }

    /// Sets the enforcement scope.
    pub fn scope(mut self, s: Scope) -> Self {
        self.scope = s;
        self
    }

    /// Tunes the number of buckets per window.
    pub fn buckets_per_window(mut self, n: i64) -> Self {
        self.buckets_per_window = n;
        self
    }

    /// Switches the rule to the exact per-event log.
    pub fn precise(mut self, p: bool) -> Self {
        self.precise = p;
        self
    }

    /// Sets a human-readable label.
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    /// The bucket width in seconds: 0 selects the exact per-event log, otherwise
    /// `window_seconds / buckets_per_window`.
    pub fn bucket_seconds(&self) -> f64 {
        if self.precise {
            return 0.0;
        }
        let buckets = self.buckets_per_window.max(1);
        self.window_seconds / buckets as f64
    }

    /// The rule's name, or a derived `model:scope:Ns` label.
    pub fn label(&self) -> String {
        match &self.name {
            Some(n) => n.clone(),
            None => format!(
                "{}:{}:{}s",
                self.model, self.scope, self.window_seconds as i64
            ),
        }
    }

    /// Whether the rule caps tokens, requests, or cost (vs concurrency only).
    pub fn has_usage_limit(&self) -> bool {
        self.max_tokens.is_some() || self.max_requests.is_some() || self.max_cost.is_some()
    }
}

/// Parses a rules document (see `limits.sample.json`) into `LimitRule`s.
pub fn rules_from_json(text: &str) -> Result<Vec<LimitRule>, String> {
    let doc: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let arr = doc
        .get("rules")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "document has no \"rules\" array".to_string())?;

    let mut out = Vec::with_capacity(arr.len());
    for row in arr {
        let model = row
            .get("model")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "rule is missing \"model\"".to_string())?;
        let window_seconds = row
            .get("window_seconds")
            .and_then(|v| v.as_f64())
            .unwrap_or(60.0);

        let mut rule = LimitRule::new(model, window_seconds);
        rule.max_tokens = row.get("max_tokens").and_then(|v| v.as_f64());
        rule.max_requests = row.get("max_requests").and_then(|v| v.as_f64());
        rule.max_cost = row.get("max_cost").and_then(|v| v.as_f64());
        rule.max_concurrent = row.get("max_concurrent").and_then(|v| v.as_i64());
        if let Some(scope) = row.get("scope").and_then(|v| v.as_str()) {
            rule.scope = scope.parse::<Scope>()?;
        }
        if let Some(buckets) = row.get("buckets_per_window").and_then(|v| v.as_i64()) {
            rule.buckets_per_window = buckets;
        }
        rule.precise = row
            .get("precise")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        rule.name = row.get("name").and_then(|v| v.as_str()).map(str::to_string);
        out.push(rule);
    }
    Ok(out)
}
