//! Reserve on the estimate, then reconcile to the actual - the standard meter model.

use quota_gate::{Acquire, LimitRule, Limiter, Scope};

fn reserve_limiter() -> Limiter {
    Limiter::new(vec![LimitRule::new("gpt-4o", 60.0)
        .max_tokens(1000.0)
        .scope(Scope::Global)])
}

#[test]
fn reserve_consumes_the_estimated_headroom() {
    let mut l = reserve_limiter();
    let (decision, res) = l.reserve("gpt-4o", &Acquire::new().tokens(800.0).now(0.0));
    assert!(decision.allowed && res.is_some());
    // 800 is reserved, so a 300-token call no longer fits.
    assert!(
        !l.try_acquire("gpt-4o", &Acquire::new().tokens(300.0).now(0.0))
            .allowed
    );
}

#[test]
fn commit_reconciles_down_and_frees_capacity() {
    let mut l = reserve_limiter();
    let (_, res) = l.reserve("gpt-4o", &Acquire::new().tokens(800.0).now(0.0));
    let mut res = res.unwrap();
    // The call actually used only 100 tokens.
    l.commit(&mut res, Some(100.0), None);
    // 900 tokens of headroom are back.
    assert!(
        l.try_acquire("gpt-4o", &Acquire::new().tokens(300.0).now(0.0))
            .allowed
    );
}

#[test]
fn commit_reconciles_up() {
    let mut l = reserve_limiter();
    let (_, res) = l.reserve("gpt-4o", &Acquire::new().tokens(100.0).now(0.0));
    let mut res = res.unwrap();
    l.commit(&mut res, Some(900.0), None);
    // Now 900 are used; a 200-token call trips the cap.
    assert!(
        !l.try_acquire("gpt-4o", &Acquire::new().tokens(200.0).now(0.0))
            .allowed
    );
}

#[test]
fn refund_returns_everything_on_a_failed_call() {
    let mut l = reserve_limiter();
    let (_, res) = l.reserve("gpt-4o", &Acquire::new().tokens(900.0).now(0.0));
    let mut res = res.unwrap();
    l.refund(&mut res);
    assert!(
        l.try_acquire("gpt-4o", &Acquire::new().tokens(1000.0).now(0.0))
            .allowed
    );
}

#[test]
fn double_commit_is_a_noop() {
    let mut l = reserve_limiter();
    let (_, res) = l.reserve("gpt-4o", &Acquire::new().tokens(100.0).now(0.0));
    let mut res = res.unwrap();
    l.commit(&mut res, Some(500.0), None);
    l.commit(&mut res, Some(999.0), None); // ignored
                                           // Used is 500, so 400 still fits but 600 does not.
    assert!(
        l.try_acquire("gpt-4o", &Acquire::new().tokens(400.0).now(0.0))
            .allowed
    );
    assert!(
        !l.try_acquire("gpt-4o", &Acquire::new().tokens(600.0).now(0.0))
            .allowed
    );
}
