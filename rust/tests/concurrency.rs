//! Concurrency slots: cap the number of in-flight requests per scope.

use quota_gate::{Acquire, LimitRule, Limiter, Scope};

fn concurrency_limiter() -> Limiter {
    Limiter::new(vec![LimitRule::new("gpt-4o", 60.0)
        .max_concurrent(2)
        .scope(Scope::Tenant)])
}

#[test]
fn slots_cap_in_flight_requests() {
    let mut l = concurrency_limiter();
    let a = l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme"));
    let b = l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme"));
    let c = l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme"));
    assert!(a.ok && b.ok);
    assert!(!c.ok);
    assert!(c.tripped_rule.is_some());
}

#[test]
fn releasing_a_slot_frees_capacity() {
    let mut l = concurrency_limiter();
    let mut a = l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme"));
    l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme"));
    assert!(!l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme")).ok);
    l.release_slot(&mut a);
    assert!(l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme")).ok);
}

#[test]
fn slots_release_on_scope_exit() {
    let mut l = concurrency_limiter();
    {
        let mut a = l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme"));
        let mut b = l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme"));
        assert!(!l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme")).ok);
        // Release both as a scope guard would on exit.
        l.release_slot(&mut a);
        l.release_slot(&mut b);
    }
    assert!(l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme")).ok);
}

#[test]
fn concurrency_is_scoped_per_tenant() {
    let mut l = concurrency_limiter();
    l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme"));
    l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme"));
    assert!(!l.acquire_slot("gpt-4o", &Acquire::new().tenant("acme")).ok);
    // A different tenant has its own concurrency budget.
    assert!(
        l.acquire_slot("gpt-4o", &Acquire::new().tenant("globex"))
            .ok
    );
}
