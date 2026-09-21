//! The limiter surface: multi-window enforcement, precise back-off, headers, cost
//! rules, graceful fallback, and config loading.

use std::collections::HashMap;

use quota_gate::{
    estimate_cost, rate_limit_headers, rules_from_json, Acquire, LimitRule, Limiter, Scope,
};

fn token_limiter() -> Limiter {
    Limiter::new(vec![LimitRule::new("gpt-4o", 60.0).max_tokens(1000.0)])
}

#[test]
fn allows_under_the_limit() {
    let mut l = token_limiter();
    let d = l.try_acquire("gpt-4o", &Acquire::new().tokens(100.0).now(0.0));
    assert!(d.allowed);
    assert_eq!(d.remaining["tokens"], 900.0);
}

#[test]
fn denies_over_token_cap_and_recovers_after_retry_after() {
    let mut l = Limiter::new(vec![LimitRule::new("gpt-4o", 60.0).max_tokens(100.0)]);
    assert!(
        l.try_acquire("gpt-4o", &Acquire::new().tokens(100.0).now(1000.0))
            .allowed
    );
    let d = l.try_acquire("gpt-4o", &Acquire::new().tokens(1.0).now(1000.0));
    assert!(!d.allowed);
    assert!(d.retry_after > 0.0);
    // Waiting exactly retry_after must actually clear the call.
    assert!(
        l.try_acquire(
            "gpt-4o",
            &Acquire::new().tokens(1.0).now(1000.0 + d.retry_after)
        )
        .allowed
    );
}

#[test]
fn denies_over_request_cap() {
    let mut l = Limiter::new(vec![LimitRule::new("gpt-4o", 60.0).max_requests(1.0)]);
    assert!(l.try_acquire("gpt-4o", &Acquire::new().now(0.0)).allowed);
    assert!(!l.try_acquire("gpt-4o", &Acquire::new().now(0.0)).allowed);
}

#[test]
fn multiple_windows_minute_ok_but_daily_trips() {
    let mut l = Limiter::new(vec![
        LimitRule::new("gpt-4o", 60.0)
            .max_requests(5.0)
            .name("per-min"),
        LimitRule::new("gpt-4o", 86_400.0)
            .max_requests(8.0)
            .name("per-day"),
    ]);
    for _ in 0..5 {
        assert!(l.try_acquire("gpt-4o", &Acquire::new().now(0.0)).allowed);
    }
    assert!(!l.try_acquire("gpt-4o", &Acquire::new().now(0.0)).allowed);
    for i in 0..3 {
        assert!(
            l.try_acquire("gpt-4o", &Acquire::new().now(61.0 + i as f64))
                .allowed
        );
    }
    let d = l.try_acquire("gpt-4o", &Acquire::new().now(64.0));
    assert!(!d.allowed);
    assert_eq!(
        d.tripped_rule.as_ref().and_then(|r| r.name.as_deref()),
        Some("per-day")
    );
}

#[test]
fn retry_after_is_precise_in_exact_mode() {
    let mut l = Limiter::new(vec![LimitRule::new("gpt-4o", 60.0)
        .max_requests(1.0)
        .precise(true)]);
    l.try_acquire("gpt-4o", &Acquire::new().now(1000.0));
    let d = l.try_acquire("gpt-4o", &Acquire::new().now(1000.0));
    assert!(
        (d.retry_after - 60.0).abs() < 1e-6,
        "expected 60, got {}",
        d.retry_after
    );
}

#[test]
fn headers_render_backpressure() {
    let mut l = Limiter::new(vec![LimitRule::new("gpt-4o", 60.0)
        .max_requests(1.0)
        .max_tokens(1000.0)]);
    l.try_acquire("gpt-4o", &Acquire::new().tokens(10.0).now(0.0));
    let d = l.try_acquire("gpt-4o", &Acquire::new().tokens(10.0).now(0.0));
    let headers = rate_limit_headers(&d, None);
    assert!(headers.contains_key("Retry-After"));
    assert_eq!(headers["X-RateLimit-Limit-Requests"], "1");
    assert_eq!(headers["X-RateLimit-Limit-Tokens"], "1000");
}

#[test]
fn cost_based_rule_caps_spend() {
    let mut l = Limiter::new(vec![LimitRule::new("gpt-4o", 60.0).max_cost(1.0)]);
    // 200k output tokens on gpt-4o = $2.00 -> over the $1 cap.
    let cost = estimate_cost("gpt-4o", 0, 200_000).unwrap();
    let d = l.try_acquire("gpt-4o", &Acquire::new().cost(cost).now(0.0));
    assert!(!d.allowed);
    assert!(d.tripped_rule.is_some());
}

#[test]
fn fallback_is_suggested_and_used_on_denial() {
    let mut fallbacks = HashMap::new();
    fallbacks.insert("gpt-4o".to_string(), "gpt-4o-mini".to_string());
    let mut l = Limiter::new(vec![
        LimitRule::new("gpt-4o", 60.0).max_requests(0.0),
        LimitRule::new("gpt-4o-mini", 60.0).max_requests(100.0),
    ])
    .with_fallbacks(fallbacks);

    let d = l.try_acquire("gpt-4o", &Acquire::new().now(0.0));
    assert!(!d.allowed);
    assert_eq!(d.suggested_fallback.as_deref(), Some("gpt-4o-mini"));

    let (used, chosen) = l.acquire_or_fallback("gpt-4o", &Acquire::new().now(0.0));
    assert!(used.allowed);
    assert_eq!(chosen, "gpt-4o-mini");
}

#[test]
fn unknown_model_has_no_rules_and_is_allowed() {
    let mut l = Limiter::new(vec![LimitRule::new("gpt-4o", 60.0).max_requests(1.0)]);
    assert!(
        l.try_acquire(
            "some-other-model",
            &Acquire::new().tokens(10_000.0).now(0.0)
        )
        .allowed
    );
}

#[test]
fn rules_load_from_json() {
    let text = r#"{ "rules": [
        { "model": "gpt-4o", "scope": "tenant", "window_seconds": 60, "max_requests": 2 }
    ] }"#;
    let rules = rules_from_json(text).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].scope, Scope::Tenant);

    let mut l = Limiter::new(rules);
    assert!(
        l.try_acquire("gpt-4o", &Acquire::new().tenant("acme").now(0.0))
            .allowed
    );
    assert!(
        l.try_acquire("gpt-4o", &Acquire::new().tenant("acme").now(0.0))
            .allowed
    );
    assert!(
        !l.try_acquire("gpt-4o", &Acquire::new().tenant("acme").now(0.0))
            .allowed
    );
}
