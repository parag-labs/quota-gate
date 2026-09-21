//! The gate itself: multi-window admission, reserve/commit/refund token
//! accounting, concurrency slots, graceful fallback, and back-pressure headers.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::rules::{LimitRule, Scope};
use crate::store::{Handle, InMemoryStore, Store, COST, REQUESTS, TOKENS};

/// The result of an admission check.
#[derive(Debug, Clone)]
pub struct Decision {
    /// Whether the call is admitted.
    pub allowed: bool,
    /// The model the decision is about.
    pub model: String,
    /// Seconds to wait before retrying (only meaningful on denial).
    pub retry_after: f64,
    /// The rule that tripped, when denied.
    pub tripped_rule: Option<LimitRule>,
    /// The scope of the tripped rule, when denied.
    pub scope: Option<Scope>,
    /// Remaining headroom per dimension, when allowed.
    pub remaining: HashMap<String, f64>,
    /// A cheaper model to fall back to, when configured and denied.
    pub suggested_fallback: Option<String>,
}

/// Optional arguments for an admission call, built fluently.
#[derive(Debug, Clone, Default)]
pub struct Acquire {
    /// Token estimate for the call.
    pub tokens: f64,
    /// Dollar-cost estimate for the call.
    pub cost: f64,
    /// Tenant the call belongs to.
    pub tenant: String,
    /// User the call belongs to (within a tenant).
    pub user: String,
    /// Explicit timestamp; falls back to the limiter clock when `None`.
    pub now: Option<f64>,
}

impl Acquire {
    /// A fresh set of default arguments.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the token estimate.
    pub fn tokens(mut self, v: f64) -> Self {
        self.tokens = v;
        self
    }

    /// Sets the cost estimate.
    pub fn cost(mut self, v: f64) -> Self {
        self.cost = v;
        self
    }

    /// Scopes the call to a tenant.
    pub fn tenant(mut self, v: &str) -> Self {
        self.tenant = v.to_string();
        self
    }

    /// Scopes the call to a user.
    pub fn user(mut self, v: &str) -> Self {
        self.user = v.to_string();
        self
    }

    /// Pins the call to an explicit timestamp.
    pub fn now(mut self, v: f64) -> Self {
        self.now = Some(v);
        self
    }
}

/// A reservation recorded up front, reconciled later with `commit` or released
/// with `refund`.
#[derive(Debug)]
pub struct Reservation {
    /// The model the reservation is for.
    pub model: String,
    /// The currently reserved token count.
    pub tokens: f64,
    /// The currently reserved cost.
    pub cost: f64,
    /// Whether the reservation has been finalised.
    pub committed: bool,
    handles: Vec<(String, Handle)>,
}

/// A held set of concurrency slots. Return them with [`Limiter::release_slot`].
#[derive(Debug)]
pub struct Slot {
    /// Whether all required slots were acquired.
    pub ok: bool,
    /// The rule that was full, when `ok` is false.
    pub tripped_rule: Option<LimitRule>,
    keys: Vec<String>,
}

fn default_clock() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn tighten(rem: &mut HashMap<String, f64>, name: &str, value: f64) {
    rem.entry(name.to_string())
        .and_modify(|v| *v = v.min(value))
        .or_insert(value);
}

/// The gate: evaluates limits before a call, reserves/reconciles tokens, caps
/// concurrency, and emits standard back-pressure signals.
pub struct Limiter {
    rules: Vec<LimitRule>,
    store: Box<dyn Store>,
    clock: Box<dyn Fn() -> f64 + Send + Sync>,
    fallbacks: HashMap<String, String>,
    by_model: HashMap<String, Vec<(usize, LimitRule)>>,
}

impl Limiter {
    /// Builds a limiter from a set of rules, with an in-memory store and wall
    /// clock by default.
    pub fn new(rules: Vec<LimitRule>) -> Self {
        let mut by_model: HashMap<String, Vec<(usize, LimitRule)>> = HashMap::new();
        for (i, r) in rules.iter().enumerate() {
            by_model
                .entry(r.model.clone())
                .or_default()
                .push((i, r.clone()));
        }
        Limiter {
            rules,
            store: Box::new(InMemoryStore::new()),
            clock: Box::new(default_clock),
            fallbacks: HashMap::new(),
            by_model,
        }
    }

    /// Plugs in a custom store.
    pub fn with_store(mut self, store: Box<dyn Store>) -> Self {
        self.store = store;
        self
    }

    /// Overrides the wall clock used when a call passes no explicit time.
    pub fn with_clock(mut self, clock: Box<dyn Fn() -> f64 + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    /// Configures per-model cheaper-model fallbacks for graceful degradation.
    pub fn with_fallbacks(mut self, fallbacks: HashMap<String, String>) -> Self {
        self.fallbacks = fallbacks;
        self
    }

    /// The rules this limiter enforces.
    pub fn rules(&self) -> &[LimitRule] {
        &self.rules
    }

    /// A shared reference to the backing store.
    pub fn store(&self) -> &dyn Store {
        self.store.as_ref()
    }

    /// A mutable reference to the backing store.
    pub fn store_mut(&mut self) -> &mut dyn Store {
        self.store.as_mut()
    }

    fn resolve_now(&self, override_now: Option<f64>) -> f64 {
        override_now.unwrap_or_else(|| (self.clock)())
    }

    fn applicable(&self, model: &str) -> Vec<(usize, LimitRule)> {
        let mut out = Vec::new();
        if let Some(v) = self.by_model.get(model) {
            out.extend(v.iter().cloned());
        }
        if let Some(v) = self.by_model.get("*") {
            out.extend(v.iter().cloned());
        }
        out
    }

    fn key(&self, index: usize, r: &LimitRule, tenant: &str, user: &str) -> String {
        match r.scope {
            Scope::Global => format!("{index}|g"),
            Scope::Tenant => format!("{index}|t|{tenant}"),
            Scope::User => format!("{index}|u|{tenant}|{user}"),
        }
    }

    fn try_acquire_inner(&mut self, model: &str, a: &Acquire, record: bool) -> Decision {
        let now = self.resolve_now(a.now);
        let rules = self.applicable(model);

        let mut worst_retry = -1.0_f64;
        let mut worst_rule: Option<LimitRule> = None;
        for (index, r) in &rules {
            if !r.has_usage_limit() {
                continue;
            }
            let key = self.key(*index, r, &a.tenant, &a.user);
            let (used_t, used_r, used_c) =
                self.store
                    .snapshot(&key, now, r.window_seconds, r.bucket_seconds());
            let mut breaches: Vec<(usize, f64)> = Vec::new();
            if let Some(mt) = r.max_tokens {
                if used_t + a.tokens > mt + 1e-9 {
                    breaches.push((TOKENS, used_t + a.tokens - mt));
                }
            }
            if let Some(mr) = r.max_requests {
                if used_r + 1.0 > mr + 1e-9 {
                    breaches.push((REQUESTS, used_r + 1.0 - mr));
                }
            }
            if let Some(mc) = r.max_cost {
                if used_c + a.cost > mc + 1e-9 {
                    breaches.push((COST, used_c + a.cost - mc));
                }
            }
            if !breaches.is_empty() {
                let mut ra = 0.0_f64;
                for (dim, over) in &breaches {
                    ra = ra.max(self.store.time_to_free(
                        &key,
                        now,
                        r.window_seconds,
                        r.bucket_seconds(),
                        *over,
                        *dim,
                    ));
                }
                if ra > worst_retry {
                    worst_retry = ra;
                    worst_rule = Some(r.clone());
                }
            }
        }

        if let Some(rule) = worst_rule {
            let scope = rule.scope;
            return Decision {
                allowed: false,
                model: model.to_string(),
                retry_after: worst_retry,
                tripped_rule: Some(rule),
                scope: Some(scope),
                remaining: HashMap::new(),
                suggested_fallback: self.fallbacks.get(model).cloned(),
            };
        }

        if record {
            for (index, r) in &rules {
                if !r.has_usage_limit() {
                    continue;
                }
                let key = self.key(*index, r, &a.tenant, &a.user);
                self.store.add(
                    &key,
                    now,
                    r.window_seconds,
                    r.bucket_seconds(),
                    [a.tokens, 1.0, a.cost],
                );
            }
        }

        let remaining = self.remaining(model, &a.tenant, &a.user, now);
        Decision {
            allowed: true,
            model: model.to_string(),
            retry_after: 0.0,
            tripped_rule: None,
            scope: None,
            remaining,
            suggested_fallback: None,
        }
    }

    fn remaining(
        &mut self,
        model: &str,
        tenant: &str,
        user: &str,
        now: f64,
    ) -> HashMap<String, f64> {
        let mut rem: HashMap<String, f64> = HashMap::new();
        for (index, r) in &self.applicable(model) {
            let key = self.key(*index, r, tenant, user);
            let (used_t, used_r, used_c) =
                self.store
                    .snapshot(&key, now, r.window_seconds, r.bucket_seconds());
            if let Some(mt) = r.max_tokens {
                tighten(&mut rem, "tokens", mt - used_t);
            }
            if let Some(mr) = r.max_requests {
                tighten(&mut rem, "requests", mr - used_r);
            }
            if let Some(mc) = r.max_cost {
                tighten(&mut rem, "cost", mc - used_c);
            }
        }
        for v in rem.values_mut() {
            *v = v.max(0.0);
        }
        rem
    }

    /// Evaluates a call against every applicable rule and, unless denied, records
    /// it.
    pub fn try_acquire(&mut self, model: &str, a: &Acquire) -> Decision {
        self.try_acquire_inner(model, a, true)
    }

    /// Records the estimate up front and returns a reservation to reconcile later.
    /// A denied call returns the denied decision and no reservation.
    pub fn reserve(&mut self, model: &str, a: &Acquire) -> (Decision, Option<Reservation>) {
        let now = self.resolve_now(a.now);
        let mut pinned = a.clone();
        pinned.now = Some(now);
        let decision = self.try_acquire_inner(model, &pinned, false);
        if !decision.allowed {
            return (decision, None);
        }
        let mut handles = Vec::new();
        for (index, r) in &self.applicable(model) {
            if !r.has_usage_limit() {
                continue;
            }
            let key = self.key(*index, r, &pinned.tenant, &pinned.user);
            let h = self.store.add(
                &key,
                now,
                r.window_seconds,
                r.bucket_seconds(),
                [pinned.tokens, 1.0, pinned.cost],
            );
            handles.push((key, h));
        }
        let reservation = Reservation {
            model: model.to_string(),
            tokens: pinned.tokens,
            cost: pinned.cost,
            committed: false,
            handles,
        };
        (decision, Some(reservation))
    }

    /// Reconciles a reservation to the actual usage. A `None` dimension is left at
    /// the reserved estimate. A committed reservation is frozen.
    pub fn commit(
        &mut self,
        res: &mut Reservation,
        actual_tokens: Option<f64>,
        actual_cost: Option<f64>,
    ) {
        if res.committed {
            return;
        }
        let dt = actual_tokens.map_or(0.0, |v| v - res.tokens);
        let dc = actual_cost.map_or(0.0, |v| v - res.cost);
        for (key, handle) in &res.handles {
            self.store.adjust(key, handle, [dt, 0.0, dc]);
        }
        res.tokens += dt;
        res.cost += dc;
        res.committed = true;
    }

    /// Releases everything a reservation recorded, e.g. after a failed call.
    pub fn refund(&mut self, res: &mut Reservation) {
        if res.committed {
            return;
        }
        for (key, handle) in &res.handles {
            self.store
                .adjust(key, handle, [-res.tokens, -1.0, -res.cost]);
        }
        res.handles.clear();
        res.committed = true;
    }

    /// Reserves one concurrency slot per applicable max-concurrent rule. If any
    /// rule is full, partially-acquired slots are released and the returned slot
    /// has `ok = false`.
    pub fn acquire_slot(&mut self, model: &str, a: &Acquire) -> Slot {
        let mut acquired: Vec<String> = Vec::new();
        for (index, r) in &self.applicable(model) {
            let Some(limit) = r.max_concurrent else {
                continue;
            };
            let key = format!("conc|{}", self.key(*index, r, &a.tenant, &a.user));
            if !self.store.try_add_concurrency(&key, limit) {
                for k in &acquired {
                    self.store.release_concurrency(k);
                }
                return Slot {
                    ok: false,
                    tripped_rule: Some(r.clone()),
                    keys: Vec::new(),
                };
            }
            acquired.push(key);
        }
        Slot {
            ok: true,
            tripped_rule: None,
            keys: acquired,
        }
    }

    /// Returns every concurrency slot a `Slot` holds.
    pub fn release_slot(&mut self, slot: &mut Slot) {
        for key in slot.keys.drain(..) {
            self.store.release_concurrency(&key);
        }
    }

    /// Tries the model, then its configured fallback on denial, reporting which
    /// model was ultimately chosen.
    pub fn acquire_or_fallback(&mut self, model: &str, a: &Acquire) -> (Decision, String) {
        let decision = self.try_acquire_inner(model, a, true);
        if decision.allowed {
            return (decision, model.to_string());
        }
        if let Some(fallback) = self.fallbacks.get(model).cloned() {
            let alt = self.try_acquire_inner(&fallback, a, true);
            if alt.allowed {
                return (alt, fallback);
            }
        }
        (decision, model.to_string())
    }
}

/// Renders a decision as the `X-RateLimit-*` / `Retry-After` headers real
/// providers return, so back-pressure can be passed straight to the caller.
pub fn rate_limit_headers(
    decision: &Decision,
    rule: Option<&LimitRule>,
) -> HashMap<String, String> {
    let rule = rule.or(decision.tripped_rule.as_ref());
    let mut headers = HashMap::new();
    if decision.retry_after > 0.0 {
        headers.insert(
            "Retry-After".to_string(),
            (decision.retry_after.ceil() as i64).to_string(),
        );
    }
    if let Some(r) = rule {
        if let Some(mr) = r.max_requests {
            headers.insert(
                "X-RateLimit-Limit-Requests".to_string(),
                (mr as i64).to_string(),
            );
        }
        if let Some(mt) = r.max_tokens {
            headers.insert(
                "X-RateLimit-Limit-Tokens".to_string(),
                (mt as i64).to_string(),
            );
        }
    }
    for (name, value) in &decision.remaining {
        let mut chars = name.chars();
        let capitalized = match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        };
        headers.insert(
            format!("X-RateLimit-Remaining-{capitalized}"),
            (*value as i64).to_string(),
        );
    }
    headers
}
