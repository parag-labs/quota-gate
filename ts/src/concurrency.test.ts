import { describe, expect, it } from "vitest";

import { Limiter } from "./limiter";
import { LimitRule, Scope } from "./rules";

// Concurrency slots: cap the number of in-flight requests per scope.

function concurrencyLimiter(): Limiter {
  return new Limiter([new LimitRule("gpt-4o", 60, { maxConcurrent: 2, scope: Scope.Tenant })]);
}

describe("concurrency", () => {
  it("caps in-flight requests with slots", () => {
    const l = concurrencyLimiter();
    const a = l.acquireSlot("gpt-4o", { tenant: "acme" });
    const b = l.acquireSlot("gpt-4o", { tenant: "acme" });
    const c = l.acquireSlot("gpt-4o", { tenant: "acme" });
    expect(a.ok).toBe(true);
    expect(b.ok).toBe(true);
    expect(c.ok).toBe(false);
    expect(c.trippedRule).not.toBeNull();
  });

  it("frees capacity when a slot is released", () => {
    const l = concurrencyLimiter();
    const a = l.acquireSlot("gpt-4o", { tenant: "acme" });
    l.acquireSlot("gpt-4o", { tenant: "acme" });
    expect(l.acquireSlot("gpt-4o", { tenant: "acme" }).ok).toBe(false);
    a.release();
    expect(l.acquireSlot("gpt-4o", { tenant: "acme" }).ok).toBe(true);
  });

  it("releases slots on scope exit (try/finally)", () => {
    const l = concurrencyLimiter();
    const a = l.acquireSlot("gpt-4o", { tenant: "acme" });
    const b = l.acquireSlot("gpt-4o", { tenant: "acme" });
    try {
      expect(l.acquireSlot("gpt-4o", { tenant: "acme" }).ok).toBe(false);
    } finally {
      a.release();
      b.release();
    }
    expect(l.acquireSlot("gpt-4o", { tenant: "acme" }).ok).toBe(true);
  });

  it("scopes concurrency per tenant", () => {
    const l = concurrencyLimiter();
    l.acquireSlot("gpt-4o", { tenant: "acme" });
    l.acquireSlot("gpt-4o", { tenant: "acme" });
    expect(l.acquireSlot("gpt-4o", { tenant: "acme" }).ok).toBe(false);
    // A different tenant has its own concurrency budget.
    expect(l.acquireSlot("gpt-4o", { tenant: "globex" }).ok).toBe(true);
  });
});
