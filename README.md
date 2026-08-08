# quota-gate

A client-side rate limiter for LLM APIs. You declare the same limits the
providers enforce - requests and tokens per rolling window, per day, per dollar -
and the gate tells you *before* each call whether to send, and if not, exactly how
long to wait. Shape your own traffic instead of discovering a `429` in production.

token-lens tells you what you spent; quota-gate stops you from overspending or
getting throttled.

## Why

OpenAI/Azure and Gemini cap you per model on several axes at once - **RPM**
(requests/min), **TPM** (tokens/min), often **RPD/TPD** per day - and enforce those
caps per org, per project, and per key. Hit any one and you get a `429` with a
`Retry-After`. Most apps meet these limits by surprise. quota-gate lets you mirror
them locally and decide up front.

## What it does

- **Many limits per model, enforced together** - a request must clear every rule
  that applies (a 60s TPM+RPM rule *and* a daily cap *and* a spend cap).
- **Hierarchical scopes** - `global` (whole fleet), `tenant` (one customer), and
  `user` (one person, e.g. "40 messages / 3h" the way ChatGPT does it). The
  toughest applicable rule wins, and the decision tells you which one and why.
- **Reserve → commit / refund** - reserve on the estimated `max_tokens` up front
  (a big requested completion counts immediately), then commit the actual usage or
  refund a failed call. This is how OpenAI's meter reconciles.
- **Concurrency slots** - cap in-flight requests per scope with a context manager.
- **Precise back-pressure** - a `retry_after` you can sleep on, plus a helper that
  renders standard `X-RateLimit-*` / `Retry-After` headers.
- **Graceful degradation** - on denial, an optional cheaper-model fallback, the way
  ChatGPT/Copilot downgrade instead of hard-failing.
- **Pluggable store** - an in-memory default; implement the same `Store` interface
  over Redis for distributed enforcement across replicas.

## The window engine

The default counter keeps **fixed-size time buckets** per key, so memory is bounded
by `window / buckets_per_window` no matter the traffic - a 24h window is ~60
counters whether you serve 10 requests or 10 million. The trailing bucket is
prorated by how much of it still lies inside the window, so accuracy is tunable.
Set `precise=true` on a rule to switch to an exact per-event log (bounded accuracy,
memory grows with traffic) when you'd rather have microsecond windows at low QPS.

## Run it (Python)

```python
from limiter import Limiter
from rules import LimitRule, Scope

limiter = Limiter([
    LimitRule("gpt-4o", scope=Scope.GLOBAL, window_seconds=60,     max_tokens=1_000_000),
    LimitRule("gpt-4o", scope=Scope.TENANT, window_seconds=60,     max_tokens=60_000, max_requests=500),
    LimitRule("gpt-4o", scope=Scope.USER,   window_seconds=10_800, max_requests=40),   # 3h, ChatGPT-style
])

d = limiter.try_acquire("gpt-4o", tokens=1_200, tenant="acme", user="parag")
if not d.allowed:
    time.sleep(d.retry_after)   # d.tripped_rule / d.scope say which limit and why
```

Reserve then reconcile:

```python
decision, res = limiter.reserve("gpt-4o", tokens=max_tokens, tenant="acme")
# ... make the call ...
limiter.commit(res, actual_tokens=usage.total_tokens)   # or limiter.refund(res) if it failed
```

Rules also load from JSON - see `limits.sample.json`:

```python
from rules import rules_from_json
limiter = Limiter(rules_from_json(open("limits.sample.json").read()))
```

Try the demo against a usage stream:

```
cd python && python src/cli.py ../limits.sample.json usage.jsonl
```

## Tests

| Language | Tests | Run |
|----------|:-----:|-----|
| Python | 28 | `cd python && pytest -q` |
| C# (.NET 10) | 23 | `cd csharp && dotnet test` |
| Java (17+) | 24 | `cd java && mvn test` |

The core is pure and dependency-light; C# and Java ports follow the same
"one behavior across languages" approach used elsewhere in parag-labs.

## Known limitations

- **Single-process by default.** The in-memory store doesn't coordinate across
  replicas. For a horizontally-scaled fleet, back the `Store` interface with Redis
  (atomic `INCR` + TTL per bucket) so counters are shared - the interface is the
  seam for exactly that.
- **Cost rules are only as good as the price table** in `pricing.py`; wire your own
  numbers (or token-lens's provider model) if you rate against negotiated pricing.

Part of [parag-labs](https://github.com/parag-labs) - small, focused tools for building AI systems you can trust.
