//! quota-gate mirrors the provider-style rate limits that model-serving APIs
//! enforce: many limits per model applied together, across global/tenant/user
//! scopes, with reserve-then-reconcile token accounting and standard
//! back-pressure signals. This is the Rust port of the Python reference.

pub mod limiter;
pub mod pricing;
pub mod rules;
pub mod store;

pub use limiter::{Acquire, Decision, Limiter, Reservation, Slot, rate_limit_headers};
pub use pricing::{PRICES, UnknownModel, estimate_cost};
pub use rules::{LimitRule, Scope, rules_from_json};
pub use store::{COST, Handle, InMemoryStore, REQUESTS, Store, TOKENS};
