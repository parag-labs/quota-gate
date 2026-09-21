import { describe, expect, it } from "vitest";

import { Limiter, Reservation } from "./limiter";
import { LimitRule, Scope } from "./rules";

function reserveLimiter(): Limiter {
  return new Limiter([new LimitRule("gpt-4o", 60, { maxTokens: 1000, scope: Scope.Global })]);
}

describe("reserve", () => {
  it("consumes the estimated headroom", () => {
    const l = reserveLimiter();
    const [decision, res] = l.reserve("gpt-4o", { tokens: 800, now: 0 });
    expect(decision.allowed).toBe(true);
    expect(res).not.toBeNull();
    // 800 is reserved, so a 300-token call no longer fits.
    expect(l.tryAcquire("gpt-4o", { tokens: 300, now: 0 }).allowed).toBe(false);
  });

  it("commits down and frees capacity", () => {
    const l = reserveLimiter();
    const [, res] = l.reserve("gpt-4o", { tokens: 800, now: 0 });
    // The call actually used only 100 tokens.
    l.commit(res as Reservation, { actualTokens: 100 });
    // 900 tokens of headroom are back.
    expect(l.tryAcquire("gpt-4o", { tokens: 300, now: 0 }).allowed).toBe(true);
  });

  it("commits up", () => {
    const l = reserveLimiter();
    const [, res] = l.reserve("gpt-4o", { tokens: 100, now: 0 });
    l.commit(res as Reservation, { actualTokens: 900 });
    // Now 900 are used; a 200-token call trips the cap.
    expect(l.tryAcquire("gpt-4o", { tokens: 200, now: 0 }).allowed).toBe(false);
  });

  it("refunds everything on a failed call", () => {
    const l = reserveLimiter();
    const [, res] = l.reserve("gpt-4o", { tokens: 900, now: 0 });
    l.refund(res as Reservation);
    expect(l.tryAcquire("gpt-4o", { tokens: 1000, now: 0 }).allowed).toBe(true);
  });

  it("treats a double commit as a no-op", () => {
    const l = reserveLimiter();
    const [, res] = l.reserve("gpt-4o", { tokens: 100, now: 0 });
    l.commit(res as Reservation, { actualTokens: 500 });
    l.commit(res as Reservation, { actualTokens: 999 }); // ignored
    // Used is 500, so 400 still fits but 600 does not.
    expect(l.tryAcquire("gpt-4o", { tokens: 400, now: 0 }).allowed).toBe(true);
    expect(l.tryAcquire("gpt-4o", { tokens: 600, now: 0 }).allowed).toBe(false);
  });
});
