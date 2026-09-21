//! quota-gate mirrors the provider-style rate limits that model-serving APIs
//! enforce: many limits per model applied together, across global/tenant/user
//! scopes, with reserve-then-reconcile token accounting and standard
//! back-pressure signals. This is the Rust port of the Python reference.

pub mod limiter;
pub mod pricing;
pub mod rules;
pub mod store;

pub use limiter::{rate_limit_headers, Acquire, Decision, Limiter, Reservation, Slot};
pub use pricing::{estimate_cost, UnknownModel, PRICES};
pub use rules::{rules_from_json, LimitRule, Scope};
pub use store::{Handle, InMemoryStore, Store, COST, REQUESTS, TOKENS};
