namespace QuotaGate;

// Dimension indices shared across the codebase.
internal static class Dim
{
    public const int Tokens = 0;
    public const int Requests = 1;
    public const int Cost = 2;
}

internal interface IWindow
{
    (double Tokens, double Requests, double Cost) Snapshot(double now);

    object Add(double now, double tokens, double requests, double cost);

    void Adjust(object handle, double dt, double dr, double dc);

    double TimeToFree(double now, double over, int dim);
}

/// <summary>Fixed-size time buckets. Memory is bounded to ~window/bucket entries.</summary>
internal sealed class BucketWindow : IWindow
{
    private readonly double _window;
    private readonly double _bucket;
    private readonly Dictionary<long, double[]> _buckets = new();

    public BucketWindow(double window, double bucket)
    {
        _window = window;
        _bucket = Math.Max(bucket, 1e-9);
    }

    public int BucketCount => _buckets.Count;

    private void Evict(double now)
    {
        double lo = now - _window;
        var dead = new List<long>();
        foreach (var idx in _buckets.Keys)
        {
            if ((idx + 1) * _bucket <= lo)
            {
                dead.Add(idx);
            }
        }

        foreach (var idx in dead)
        {
            _buckets.Remove(idx);
        }
    }

    public (double, double, double) Snapshot(double now)
    {
        double lo = now - _window;
        double t = 0, r = 0, c = 0;
        foreach (var (idx, v) in _buckets)
        {
            double bStart = idx * _bucket;
            double bEnd = bStart + _bucket;
            if (bEnd <= lo)
            {
                continue;
            }

            double frac = bStart >= lo ? 1.0 : (bEnd - lo) / _bucket;
            t += v[Dim.Tokens] * frac;
            r += v[Dim.Requests] * frac;
            c += v[Dim.Cost] * frac;
        }

        return (t, r, c);
    }

    public object Add(double now, double tokens, double requests, double cost)
    {
        Evict(now);
        long idx = (long)Math.Floor(now / _bucket);
        if (!_buckets.TryGetValue(idx, out var v))
        {
            v = new double[3];
            _buckets[idx] = v;
        }

        v[Dim.Tokens] += tokens;
        v[Dim.Requests] += requests;
        v[Dim.Cost] += cost;
        return idx;
    }

    public void Adjust(object handle, double dt, double dr, double dc)
    {
        long idx = (long)handle;
        if (_buckets.TryGetValue(idx, out var v))
        {
            v[Dim.Tokens] += dt;
            v[Dim.Requests] += dr;
            v[Dim.Cost] += dc;
        }
    }

    public double TimeToFree(double now, double over, int dim)
    {
        double lo = now - _window;
        var entries = new List<(double bEnd, double amt)>();
        foreach (var (idx, v) in _buckets)
        {
            double bEnd = (idx + 1) * _bucket;
            if (bEnd <= lo || v[dim] <= 0)
            {
                continue;
            }

            entries.Add((bEnd, v[dim]));
        }

        entries.Sort((a, b) => a.bEnd.CompareTo(b.bEnd));
        double freed = 0;
        foreach (var e in entries)
        {
            freed += e.amt;
            if (freed >= over - 1e-9)
            {
                return Math.Max(0.0, e.bEnd + _window - now);
            }
        }

        return _window;
    }
}

/// <summary>Exact per-event log. Memory grows with in-window traffic.</summary>
internal sealed class PreciseWindow : IWindow
{
    private readonly double _window;
    private List<double[]> _events = new(); // {ts, tokens, requests, cost}

    public PreciseWindow(double window)
    {
        _window = window;
    }

    private void Evict(double now)
    {
        double lo = now - _window;
        _events = _events.Where(e => e[0] > lo).ToList();
    }

    public (double, double, double) Snapshot(double now)
    {
        double lo = now - _window;
        double t = 0, r = 0, c = 0;
        foreach (var e in _events)
        {
            if (e[0] > lo)
            {
                t += e[1];
                r += e[2];
                c += e[3];
            }
        }

        return (t, r, c);
    }

    public object Add(double now, double tokens, double requests, double cost)
    {
        Evict(now);
        var e = new[] { now, tokens, requests, cost };
        _events.Add(e);
        return e;
    }

    public void Adjust(object handle, double dt, double dr, double dc)
    {
        var e = (double[])handle;
        e[1] += dt;
        e[2] += dr;
        e[3] += dc;
    }

    public double TimeToFree(double now, double over, int dim)
    {
        double lo = now - _window;
        var live = _events.Where(e => e[0] > lo).OrderBy(e => e[0]);
        double freed = 0;
        foreach (var e in live)
        {
            freed += e[dim + 1];
            if (freed >= over - 1e-9)
            {
                return Math.Max(0.0, e[0] + _window - now);
            }
        }

        return _window;
    }
}

/// <summary>The persistence seam. Implement over Redis for distributed enforcement.</summary>
public interface IStore
{
    (double Tokens, double Requests, double Cost) Snapshot(string key, double now, double window, double bucket);

    object Add(string key, double now, double window, double bucket, double tokens, double requests, double cost);

    void Adjust(string key, object handle, double dt, double dr, double dc);

    double TimeToFree(string key, double now, double window, double bucket, double over, int dim);

    int Concurrency(string key);

    bool TryAddConcurrency(string key, int limit);

    void ReleaseConcurrency(string key);
}

/// <summary>Single-process default. One window object and one counter per key.</summary>
public sealed class InMemoryStore : IStore
{
    private readonly Dictionary<string, IWindow> _windows = new();
    private readonly Dictionary<string, int> _concurrency = new();

    private IWindow Window(string key, double window, double bucket)
    {
        if (!_windows.TryGetValue(key, out var w))
        {
            w = bucket <= 0 ? new PreciseWindow(window) : new BucketWindow(window, bucket);
            _windows[key] = w;
        }

        return w;
    }

    public (double, double, double) Snapshot(string key, double now, double window, double bucket)
        => Window(key, window, bucket).Snapshot(now);

    public object Add(string key, double now, double window, double bucket,
        double tokens, double requests, double cost)
        => Window(key, window, bucket).Add(now, tokens, requests, cost);

    public void Adjust(string key, object handle, double dt, double dr, double dc)
    {
        if (_windows.TryGetValue(key, out var w))
        {
            w.Adjust(handle, dt, dr, dc);
        }
    }

    public double TimeToFree(string key, double now, double window, double bucket, double over, int dim)
        => Window(key, window, bucket).TimeToFree(now, over, dim);

    public int Concurrency(string key) => _concurrency.TryGetValue(key, out var v) ? v : 0;

    public bool TryAddConcurrency(string key, int limit)
    {
        int cur = Concurrency(key);
        if (cur >= limit)
        {
            return false;
        }

        _concurrency[key] = cur + 1;
        return true;
    }

    public void ReleaseConcurrency(string key)
    {
        int cur = Concurrency(key);
        if (cur > 0)
        {
            _concurrency[key] = cur - 1;
        }
    }
}
