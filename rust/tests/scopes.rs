//! Hierarchical scopes: global fleet, per-tenant, per-user - all enforced together.

use quota_gate::{Acquire, LimitRule, Limiter, Scope};

fn scoped_limiter(max_requests: f64, scope: Scope) -> Limiter {
    Limiter::new(vec![LimitRule::new("gpt-4o", 60.0)
        .max_requests(max_requests)
        .scope(scope)])
}

#[test]
fn tenant_scope_isolates_tenants() {
    let mut l = scoped_limiter(2.0, Scope::Tenant);
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
    // A different tenant has its own budget.
    assert!(
        l.try_acquire("gpt-4o", &Acquire::new().tenant("globex").now(0.0))
            .allowed
    );
}

#[test]
fn user_scope_isolates_users_within_a_tenant() {
    let mut l = scoped_limiter(1.0, Scope::User);
    assert!(
        l.try_acquire(
            "gpt-4o",
            &Acquire::new().tenant("acme").user("ann").now(0.0)
        )
        .allowed
    );
    assert!(
        !l.try_acquire(
            "gpt-4o",
            &Acquire::new().tenant("acme").user("ann").now(0.0)
        )
        .allowed
    );
    // Same tenant, different user.
    assert!(
        l.try_acquire(
            "gpt-4o",
            &Acquire::new().tenant("acme").user("bob").now(0.0)
        )
        .allowed
    );
}

#[test]
fn global_scope_is_shared_across_everyone() {
    let mut l = scoped_limiter(2.0, Scope::Global);
    assert!(
        l.try_acquire(
            "gpt-4o",
            &Acquire::new().tenant("acme").user("ann").now(0.0)
        )
        .allowed
    );
    assert!(
        l.try_acquire(
            "gpt-4o",
            &Acquire::new().tenant("globex").user("bob").now(0.0)
        )
        .allowed
    );
    // The fleet cap is hit no matter who is calling.
    assert!(
        !l.try_acquire("gpt-4o", &Acquire::new().tenant("new").user("cat").now(0.0))
            .allowed
    );
}

#[test]
fn toughest_applicable_rule_wins() {
    // A generous per-tenant cap but a tiny global fleet cap.
    let mut l = Limiter::new(vec![
        LimitRule::new("gpt-4o", 60.0)
            .max_requests(1000.0)
            .scope(Scope::Tenant)
            .name("tenant"),
        LimitRule::new("gpt-4o", 60.0)
            .max_requests(1.0)
            .scope(Scope::Global)
            .name("fleet"),
    ]);
    assert!(
        l.try_acquire("gpt-4o", &Acquire::new().tenant("acme").now(0.0))
            .allowed
    );
    let d = l.try_acquire("gpt-4o", &Acquire::new().tenant("acme").now(0.0));
    assert!(!d.allowed);
    assert_eq!(
        d.tripped_rule.as_ref().and_then(|r| r.name.as_deref()),
        Some("fleet")
    );
    assert_eq!(d.scope, Some(Scope::Global));
}
