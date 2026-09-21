import { describe, expect, it } from "vitest";

import { Limiter, rateLimitHeaders } from "./limiter";
import { estimateCost } from "./pricing";
import { LimitRule, Scope, rulesFromJson } from "./rules";

describe("limiter", () => {
  it("allows a call under the limit and reports remaining headroom", () => {
    const l = new Limiter([new LimitRule("gpt-4o", 60, { maxTokens: 1000 })]);
    const d = l.tryAcquire("gpt-4o", { tokens: 100, now: 0 });
    expect(d.allowed).toBe(true);
    expect(d.remaining.tokens).toBe(900);
  });

  it("denies over the token cap and recovers after retryAfter", () => {
    const l = new Limiter([new LimitRule("gpt-4o", 60, { maxTokens: 100 })]);
    expect(l.tryAcquire("gpt-4o", { tokens: 100, now: 1000 }).allowed).toBe(true);
    const d = l.tryAcquire("gpt-4o", { tokens: 1, now: 1000 });
    expect(d.allowed).toBe(false);
    expect(d.retryAfter).toBeGreaterThan(0);
    // Waiting exactly retryAfter must actually clear the call.
    expect(l.tryAcquire("gpt-4o", { tokens: 1, now: 1000 + d.retryAfter }).allowed).toBe(true);
  });

  it("denies over the request cap", () => {
    const l = new Limiter([new LimitRule("gpt-4o", 60, { maxRequests: 1 })]);
    expect(l.tryAcquire("gpt-4o", { now: 0 }).allowed).toBe(true);
    expect(l.tryAcquire("gpt-4o", { now: 0 }).allowed).toBe(false);
  });

  it("enforces multiple windows: per-minute passes but the daily cap trips", () => {
    const l = new Limiter([
      new LimitRule("gpt-4o", 60, { maxRequests: 5, name: "per-min" }),
      new LimitRule("gpt-4o", 86_400, { maxRequests: 8, name: "per-day" }),
    ]);
    for (let i = 0; i < 5; i++) {
      expect(l.tryAcquire("gpt-4o", { now: 0 }).allowed).toBe(true);
    }
    expect(l.tryAcquire("gpt-4o", { now: 0 }).allowed).toBe(false); // per-min tripped
    for (let i = 0; i < 3; i++) {
      expect(l.tryAcquire("gpt-4o", { now: 61 + i }).allowed).toBe(true);
    }
    const d = l.tryAcquire("gpt-4o", { now: 64 });
    expect(d.allowed).toBe(false);
    expect(d.trippedRule?.name).toBe("per-day");
  });

  it("gives a precise retryAfter in exact mode", () => {
    const l = new Limiter([new LimitRule("gpt-4o", 60, { maxRequests: 1, precise: true })]);
    l.tryAcquire("gpt-4o", { now: 1000 });
    const d = l.tryAcquire("gpt-4o", { now: 1000 });
    // Event at 1000 leaves the 60s window at 1060 -> retryAfter == 60.
    expect(Math.abs(d.retryAfter - 60)).toBeLessThan(1e-6);
  });

  it("renders back-pressure headers", () => {
    const l = new Limiter([new LimitRule("gpt-4o", 60, { maxRequests: 1, maxTokens: 1000 })]);
    l.tryAcquire("gpt-4o", { tokens: 10, now: 0 });
    const d = l.tryAcquire("gpt-4o", { tokens: 10, now: 0 });
    const headers = rateLimitHeaders(d);
    expect("Retry-After" in headers).toBe(true);
    expect(headers["X-RateLimit-Limit-Requests"]).toBe("1");
    expect(headers["X-RateLimit-Limit-Tokens"]).toBe("1000");
  });

  it("caps spend with a cost-based rule", () => {
    const l = new Limiter([new LimitRule("gpt-4o", 60, { maxCost: 1.0 })]);
    // 200k output tokens on gpt-4o = 200000/1e6 * 10 = $2.00 -> over the $1 cap.
    const cost = estimateCost("gpt-4o", 0, 200_000);
    const d = l.tryAcquire("gpt-4o", { cost, now: 0 });
    expect(d.allowed).toBe(false);
    expect(d.trippedRule).not.toBeNull();
  });

  it("suggests and uses a fallback on denial", () => {
    const l = new Limiter(
      [
        new LimitRule("gpt-4o", 60, { maxRequests: 0 }), // always full
        new LimitRule("gpt-4o-mini", 60, { maxRequests: 100 }),
      ],
      { fallbacks: { "gpt-4o": "gpt-4o-mini" } },
    );
    const d = l.tryAcquire("gpt-4o", { now: 0 });
    expect(d.allowed).toBe(false);
    expect(d.suggestedFallback).toBe("gpt-4o-mini");

    const [used, chosen] = l.acquireOrFallback("gpt-4o", { now: 0 });
    expect(used.allowed).toBe(true);
    expect(chosen).toBe("gpt-4o-mini");
  });

  it("lets an unknown model with no rules pass through (fail-open)", () => {
    const l = new Limiter([new LimitRule("gpt-4o", 60, { maxRequests: 1 })]);
    expect(l.tryAcquire("some-other-model", { tokens: 10_000, now: 0 }).allowed).toBe(true);
  });

  it("loads rules from JSON", () => {
    const text = `{ "rules": [
      { "model": "gpt-4o", "scope": "tenant", "window_seconds": 60, "max_requests": 2 }
    ] }`;
    const rules = rulesFromJson(text);
    expect(rules).toHaveLength(1);
    expect(rules[0].scope).toBe(Scope.Tenant);

    const l = new Limiter(rules);
    expect(l.tryAcquire("gpt-4o", { tenant: "acme", now: 0 }).allowed).toBe(true);
    expect(l.tryAcquire("gpt-4o", { tenant: "acme", now: 0 }).allowed).toBe(true);
    expect(l.tryAcquire("gpt-4o", { tenant: "acme", now: 0 }).allowed).toBe(false);
  });
});
