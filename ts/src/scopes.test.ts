import { describe, expect, it } from "vitest";

import { Limiter } from "./limiter";
import { LimitRule, Scope } from "./rules";

// Hierarchical scopes: global fleet, per-tenant, per-user - all enforced together.

describe("scopes", () => {
  it("isolates tenants under a tenant-scoped rule", () => {
    const l = new Limiter([new LimitRule("gpt-4o", 60, { maxRequests: 2, scope: Scope.Tenant })]);
    expect(l.tryAcquire("gpt-4o", { tenant: "acme", now: 0 }).allowed).toBe(true);
    expect(l.tryAcquire("gpt-4o", { tenant: "acme", now: 0 }).allowed).toBe(true);
    expect(l.tryAcquire("gpt-4o", { tenant: "acme", now: 0 }).allowed).toBe(false);
    // A different tenant has its own budget.
    expect(l.tryAcquire("gpt-4o", { tenant: "globex", now: 0 }).allowed).toBe(true);
  });

  it("isolates users within a tenant under a user-scoped rule", () => {
    const l = new Limiter([new LimitRule("gpt-4o", 60, { maxRequests: 1, scope: Scope.User })]);
    expect(l.tryAcquire("gpt-4o", { tenant: "acme", user: "ann", now: 0 }).allowed).toBe(true);
    expect(l.tryAcquire("gpt-4o", { tenant: "acme", user: "ann", now: 0 }).allowed).toBe(false);
    // Same tenant, different user.
    expect(l.tryAcquire("gpt-4o", { tenant: "acme", user: "bob", now: 0 }).allowed).toBe(true);
  });

  it("shares a global-scoped rule across everyone", () => {
    const l = new Limiter([new LimitRule("gpt-4o", 60, { maxRequests: 2, scope: Scope.Global })]);
    expect(l.tryAcquire("gpt-4o", { tenant: "acme", user: "ann", now: 0 }).allowed).toBe(true);
    expect(l.tryAcquire("gpt-4o", { tenant: "globex", user: "bob", now: 0 }).allowed).toBe(true);
    // The fleet cap is hit no matter who is calling.
    expect(l.tryAcquire("gpt-4o", { tenant: "new", user: "cat", now: 0 }).allowed).toBe(false);
  });

  it("lets the toughest applicable rule win", () => {
    // A generous per-tenant cap but a tiny global fleet cap.
    const l = new Limiter([
      new LimitRule("gpt-4o", 60, { maxRequests: 1000, scope: Scope.Tenant, name: "tenant" }),
      new LimitRule("gpt-4o", 60, { maxRequests: 1, scope: Scope.Global, name: "fleet" }),
    ]);
    expect(l.tryAcquire("gpt-4o", { tenant: "acme", now: 0 }).allowed).toBe(true);
    const d = l.tryAcquire("gpt-4o", { tenant: "acme", now: 0 });
    expect(d.allowed).toBe(false);
    expect(d.trippedRule?.name).toBe("fleet");
    expect(d.scope).toBe(Scope.Global);
  });
});
