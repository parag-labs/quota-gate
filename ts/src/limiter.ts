/**
 * The gate: evaluate provider-style limits before a call, reserve/reconcile
 * tokens, cap concurrency, and emit standard back-pressure signals.
 *
 * Design mirrors how model-serving APIs meter in practice:
 *
 * - Multiple limits per model enforced *together* (RPM + TPM + daily + cost).
 * - Hierarchical *scopes*: a request must clear the global fleet cap, its tenant's
 *   cap, and the user's cap - the toughest applicable rule wins.
 * - Reserve on estimated `maxTokens`, then commit the actual usage (or refund a
 *   failed call), so a large requested completion counts against you immediately
 *   and is reconciled afterwards.
 * - Concurrency slots cap in-flight requests per scope.
 * - On denial, a precise `retryAfter` and an optional cheaper-model fallback, so
 *   callers can degrade gracefully instead of hard-failing.
 */

import { LimitRule, Scope } from "./rules";
import { COST, Handle, InMemoryStore, REQUESTS, Store, TOKENS } from "./store";

/** The result of an admission check. */
export interface Decision {
  allowed: boolean;
  model: string;
  retryAfter: number;
  trippedRule: LimitRule | null;
  scope: Scope | null;
  remaining: Record<string, number>;
  suggestedFallback: string | null;
}

/**
 * Records counters up front so the actual usage can be reconciled (commit) or
 * released (refund) afterwards.
 */
export interface Reservation {
  model: string;
  tokens: number;
  cost: number;
  committed: boolean;
  handles: Array<[string, Handle]>;
}

/** The optional arguments of an admission call. */
export interface AcquireOptions {
  tokens?: number;
  cost?: number;
  tenant?: string;
  user?: string;
  now?: number;
}

/** The actual usage passed to {@link Limiter.commit}. */
export interface CommitOptions {
  actualTokens?: number;
  actualCost?: number;
}

/** Options for constructing a {@link Limiter}. */
export interface LimiterOptions {
  store?: Store;
  clock?: () => number;
  fallbacks?: Record<string, string>;
}

/**
 * A held concurrency reservation. Call {@link Slot.release} (e.g. in a `finally`)
 * to give the in-flight slots back.
 */
export class Slot {
  ok: boolean;
  trippedRule: LimitRule | null;
  private readonly limiter: Limiter;
  private keys: string[];

  constructor(limiter: Limiter, keys: string[], ok: boolean, trippedRule: LimitRule | null = null) {
    this.limiter = limiter;
    this.keys = keys;
    this.ok = ok;
    this.trippedRule = trippedRule;
  }

  /** Returns every concurrency slot this Slot holds. */
  release(): void {
    for (const k of this.keys) {
      this.limiter.store.releaseConcurrency(k);
    }
    this.keys = [];
  }
}

/**
 * The gate: it evaluates limits before a call, reserves/reconciles tokens, caps
 * concurrency, and emits standard back-pressure signals.
 */
export class Limiter {
  readonly rules: LimitRule[];
  readonly store: Store;
  private readonly clock: () => number;
  private readonly fallbacks: Record<string, string>;
  private readonly byModel = new Map<string, Array<[number, LimitRule]>>();

  constructor(rules: LimitRule[], opts: LimiterOptions = {}) {
    this.rules = [...rules];
    this.store = opts.store ?? new InMemoryStore();
    this.clock = opts.clock ?? (() => Date.now() / 1000);
    this.fallbacks = { ...(opts.fallbacks ?? {}) };
    this.rules.forEach((r, i) => {
      const list = this.byModel.get(r.model) ?? [];
      list.push([i, r]);
      this.byModel.set(r.model, list);
    });
  }

  private applicable(model: string): Array<[number, LimitRule]> {
    return [...(this.byModel.get(model) ?? []), ...(this.byModel.get("*") ?? [])];
  }

  private key(index: number, rule: LimitRule, tenant: string, user: string): string {
    if (rule.scope === Scope.Global) {
      return `${index}|g`;
    }
    if (rule.scope === Scope.Tenant) {
      return `${index}|t|${tenant}`;
    }
    return `${index}|u|${tenant}|${user}`;
  }

  private resolveNow(now: number | undefined): number {
    return now === undefined ? this.clock() : now;
  }

  /**
   * Evaluates a call against every applicable rule and, unless denied, records
   * it. On denial the Decision carries the retryAfter, the tripped rule, its
   * scope, and any configured fallback.
   */
  tryAcquire(model: string, opts: AcquireOptions = {}): Decision {
    return this.tryAcquireInner(model, opts, true);
  }

  private tryAcquireInner(model: string, opts: AcquireOptions, record: boolean): Decision {
    const now = this.resolveNow(opts.now);
    const tokens = opts.tokens ?? 0;
    const cost = opts.cost ?? 0;
    const tenant = opts.tenant ?? "";
    const user = opts.user ?? "";
    const rules = this.applicable(model);

    let worstRetry = -1;
    let worstRule: LimitRule | null = null;
    for (const [i, r] of rules) {
      if (!r.hasUsageLimit()) {
        continue; // e.g. a concurrency-only rule
      }
      const key = this.key(i, r, tenant, user);
      const [usedT, usedR, usedC] = this.store.snapshot(key, now, r.windowSeconds, r.bucketSeconds);
      const breached: Array<[number, number]> = [];
      if (r.maxTokens !== undefined && usedT + tokens > r.maxTokens + 1e-9) {
        breached.push([TOKENS, usedT + tokens - r.maxTokens]);
      }
      if (r.maxRequests !== undefined && usedR + 1 > r.maxRequests + 1e-9) {
        breached.push([REQUESTS, usedR + 1 - r.maxRequests]);
      }
      if (r.maxCost !== undefined && usedC + cost > r.maxCost + 1e-9) {
        breached.push([COST, usedC + cost - r.maxCost]);
      }
      if (breached.length > 0) {
        let ra = 0;
        for (const [dim, over] of breached) {
          ra = Math.max(ra, this.store.timeToFree(key, now, r.windowSeconds, r.bucketSeconds, over, dim));
        }
        if (ra > worstRetry) {
          worstRetry = ra;
          worstRule = r;
        }
      }
    }

    if (worstRule !== null) {
      return {
        allowed: false,
        model,
        retryAfter: worstRetry,
        trippedRule: worstRule,
        scope: worstRule.scope,
        remaining: {},
        suggestedFallback: this.fallbacks[model] ?? null,
      };
    }

    if (record) {
      for (const [i, r] of rules) {
        if (!r.hasUsageLimit()) {
          continue;
        }
        const key = this.key(i, r, tenant, user);
        this.store.add(key, now, r.windowSeconds, r.bucketSeconds, tokens, 1.0, cost);
      }
    }

    return {
      allowed: true,
      model,
      retryAfter: 0,
      trippedRule: null,
      scope: null,
      remaining: this.remaining(model, tenant, user, now),
      suggestedFallback: null,
    };
  }

  private remaining(model: string, tenant: string, user: string, now: number): Record<string, number> {
    const rem: Record<string, number> = {};
    const tighten = (name: string, value: number): void => {
      rem[name] = Math.min(rem[name] ?? Infinity, value);
    };
    for (const [i, r] of this.applicable(model)) {
      const key = this.key(i, r, tenant, user);
      const [usedT, usedR, usedC] = this.store.snapshot(key, now, r.windowSeconds, r.bucketSeconds);
      if (r.maxTokens !== undefined) {
        tighten("tokens", r.maxTokens - usedT);
      }
      if (r.maxRequests !== undefined) {
        tighten("requests", r.maxRequests - usedR);
      }
      if (r.maxCost !== undefined) {
        tighten("cost", r.maxCost - usedC);
      }
    }
    for (const k of Object.keys(rem)) {
      rem[k] = Math.max(0, rem[k]);
    }
    return rem;
  }

  // ---- reserve -> commit / refund ----

  /**
   * Records the estimate up front and returns a Reservation to reconcile later.
   * If the call would be denied, it returns the denied Decision and a null
   * Reservation.
   */
  reserve(model: string, opts: AcquireOptions = {}): [Decision, Reservation | null] {
    const now = this.resolveNow(opts.now);
    const decision = this.tryAcquireInner(model, { ...opts, now }, false);
    if (!decision.allowed) {
      return [decision, null];
    }
    const tokens = opts.tokens ?? 0;
    const cost = opts.cost ?? 0;
    const tenant = opts.tenant ?? "";
    const user = opts.user ?? "";
    const handles: Array<[string, Handle]> = [];
    for (const [i, r] of this.applicable(model)) {
      if (!r.hasUsageLimit()) {
        continue;
      }
      const key = this.key(i, r, tenant, user);
      const h = this.store.add(key, now, r.windowSeconds, r.bucketSeconds, tokens, 1.0, cost);
      handles.push([key, h]);
    }
    return [decision, { model, tokens, cost, committed: false, handles }];
  }

  /**
   * Reconciles a reservation to the actual usage. An undefined actualTokens or
   * actualCost leaves that dimension at the reserved estimate. A committed
   * reservation is frozen and further commits/refunds are no-ops.
   */
  commit(res: Reservation, opts: CommitOptions = {}): void {
    if (res.committed) {
      return;
    }
    const dt = opts.actualTokens === undefined ? 0 : opts.actualTokens - res.tokens;
    const dc = opts.actualCost === undefined ? 0 : opts.actualCost - res.cost;
    for (const [key, handle] of res.handles) {
      this.store.adjust(key, handle, dt, 0.0, dc);
    }
    res.tokens += dt;
    res.cost += dc;
    res.committed = true;
  }

  /** Releases everything a reservation recorded, e.g. after a failed call. */
  refund(res: Reservation): void {
    if (res.committed) {
      return;
    }
    for (const [key, handle] of res.handles) {
      this.store.adjust(key, handle, -res.tokens, -1.0, -res.cost);
    }
    res.handles = [];
    res.committed = true;
  }

  // ---- concurrency slots ----

  /**
   * Reserves one concurrency slot per applicable max-concurrent rule. If any rule
   * is full, all partially-acquired slots are released and the returned Slot has
   * ok=false with the tripped rule.
   */
  acquireSlot(model: string, opts: AcquireOptions = {}): Slot {
    const tenant = opts.tenant ?? "";
    const user = opts.user ?? "";
    const acquired: string[] = [];
    for (const [i, r] of this.applicable(model)) {
      if (r.maxConcurrent === undefined) {
        continue;
      }
      const key = "conc|" + this.key(i, r, tenant, user);
      if (!this.store.tryAddConcurrency(key, r.maxConcurrent)) {
        for (const k of acquired) {
          this.store.releaseConcurrency(k);
        }
        return new Slot(this, [], false, r);
      }
      acquired.push(key);
    }
    return new Slot(this, acquired, true, null);
  }

  // ---- graceful degradation ----

  /**
   * Tries the model, then its configured fallback on denial, and reports which
   * model was ultimately chosen.
   */
  acquireOrFallback(model: string, opts: AcquireOptions = {}): [Decision, string] {
    const decision = this.tryAcquire(model, opts);
    if (decision.allowed) {
      return [decision, model];
    }
    const fallback = this.fallbacks[model];
    if (fallback !== undefined) {
      const alt = this.tryAcquire(fallback, opts);
      if (alt.allowed) {
        return [alt, fallback];
      }
    }
    return [decision, model];
  }
}

/**
 * Renders a decision as the `X-RateLimit-*` / `Retry-After` headers real
 * providers return, so you can pass back-pressure straight to your caller.
 */
export function rateLimitHeaders(decision: Decision, rule: LimitRule | null = null): Record<string, string> {
  const r = rule ?? decision.trippedRule;
  const headers: Record<string, string> = {};
  if (decision.retryAfter > 0) {
    headers["Retry-After"] = String(Math.ceil(decision.retryAfter));
  }
  if (r !== null) {
    if (r.maxRequests !== undefined) {
      headers["X-RateLimit-Limit-Requests"] = String(Math.trunc(r.maxRequests));
    }
    if (r.maxTokens !== undefined) {
      headers["X-RateLimit-Limit-Tokens"] = String(Math.trunc(r.maxTokens));
    }
  }
  for (const [name, value] of Object.entries(decision.remaining)) {
    const capitalized = name.charAt(0).toUpperCase() + name.slice(1);
    headers[`X-RateLimit-Remaining-${capitalized}`] = String(Math.trunc(value));
  }
  return headers;
}
