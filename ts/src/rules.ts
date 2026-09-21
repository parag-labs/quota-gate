/** Rule definitions: what to limit, over which window, at which scope. */

/**
 * Where a limit is enforced. A request is keyed differently per scope so the same
 * rule isolates one global fleet, one tenant, or one user.
 */
export enum Scope {
  Global = "global",
  Tenant = "tenant",
  User = "user",
}

/** Maps a wire name to a {@link Scope}, throwing for unknown names. */
export function parseScope(s: string): Scope {
  switch (s) {
    case "global":
      return Scope.Global;
    case "tenant":
      return Scope.Tenant;
    case "user":
      return Scope.User;
    default:
      throw new Error(`unknown scope ${s}`);
  }
}

/** The optional dimensions and tuning knobs of a {@link LimitRule}. */
export interface LimitRuleOptions {
  maxTokens?: number;
  maxRequests?: number;
  maxCost?: number;
  maxConcurrent?: number;
  scope?: Scope;
  bucketsPerWindow?: number;
  precise?: boolean;
  name?: string;
}

/**
 * One provider-style limit. Any subset of the `max*` dimensions may be set; a
 * request must stay under every one that is.
 */
export class LimitRule {
  readonly model: string;
  readonly windowSeconds: number;
  readonly maxTokens?: number;
  readonly maxRequests?: number;
  readonly maxCost?: number;
  readonly maxConcurrent?: number;
  readonly scope: Scope;
  readonly bucketsPerWindow: number;
  readonly precise: boolean;
  readonly name?: string;

  constructor(model: string, windowSeconds: number, opts: LimitRuleOptions = {}) {
    this.model = model;
    this.windowSeconds = windowSeconds;
    this.maxTokens = opts.maxTokens;
    this.maxRequests = opts.maxRequests;
    this.maxCost = opts.maxCost;
    this.maxConcurrent = opts.maxConcurrent;
    this.scope = opts.scope ?? Scope.Global;
    this.bucketsPerWindow = opts.bucketsPerWindow ?? 60;
    this.precise = opts.precise ?? false;
    this.name = opts.name;
  }

  /** 0 selects the exact per-event log; otherwise window/bucketsPerWindow. */
  get bucketSeconds(): number {
    if (this.precise) {
      return 0;
    }
    return this.windowSeconds / Math.max(1, this.bucketsPerWindow);
  }

  /** The rule's name, or a derived `model:scope:Ns` label. */
  get label(): string {
    return this.name ?? `${this.model}:${this.scope}:${Math.trunc(this.windowSeconds)}s`;
  }

  /**
   * Whether the rule caps tokens, requests, or cost (as opposed to being a
   * concurrency-only rule).
   */
  hasUsageLimit(): boolean {
    return this.maxTokens !== undefined || this.maxRequests !== undefined || this.maxCost !== undefined;
  }
}

interface RuleRow {
  model: string;
  window_seconds: number;
  max_tokens?: number;
  max_requests?: number;
  max_cost?: number;
  max_concurrent?: number;
  scope?: string;
  buckets_per_window?: number;
  precise?: boolean;
  name?: string;
}

/** Parses a rules document (see limits.sample.json) into {@link LimitRule}s. */
export function rulesFromJson(text: string): LimitRule[] {
  const doc = JSON.parse(text) as { rules: RuleRow[] };
  return doc.rules.map(
    (row) =>
      new LimitRule(row.model, Number(row.window_seconds), {
        maxTokens: row.max_tokens,
        maxRequests: row.max_requests,
        maxCost: row.max_cost,
        maxConcurrent: row.max_concurrent,
        scope: row.scope === undefined ? Scope.Global : parseScope(row.scope),
        bucketsPerWindow: row.buckets_per_window === undefined ? 60 : Number(row.buckets_per_window),
        precise: row.precise === undefined ? false : Boolean(row.precise),
        name: row.name,
      }),
  );
}
