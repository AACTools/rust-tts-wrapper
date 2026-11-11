using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using RustTtsWrapper;
using Xunit;

namespace RustTtsWrapper.Bindings.Tests;

/// <summary>
/// Request-scheduler semantics (GH #70) exercised against a fake
/// backend — no native library needed: queue/replace dispatch, stop
/// behaviour, cancellation, and the callback-isolation guarantee
/// ("a cancelled or superseded request cannot deliver callbacks to a
/// newer request").
/// </summary>
public sealed class SpeechRequestSchedulerTests : IDisposable
{
    /// <summary>Scriptable fake engine.</summary>
    private sealed class FakeBackend : ISpeechBackend
    {
        private Action<string, int, int, float, float, bool>? _handler;

        public ManualResetEventSlim Speaking = new(false);
        public volatile bool StopRequested;
        public int StopCalls;
        public List<string> Spoken { get; } = new();
        public int SpeakMillis { get; set; } = 200;
        /// <summary>Words the fake engine emits per utterance.</summary>
        public string[] Words { get; set; } = { "hello", "world" };

        public void SpeakSync(string text)
        {
            // A new utterance starts fresh: stop requests target the
            // in-flight synthesis only (mirrors real engines).
            StopRequested = false;
            lock (Spoken) Spoken.Add(text);
            Speaking.Set();
            var sw = System.Diagnostics.Stopwatch.StartNew();
            // Emit words while "synthesizing", honouring stop.
            foreach (var w in Words)
            {
                if (StopRequested) return;
                _handler?.Invoke(w, -1, -1, 0.1f, 0.2f, true);
                Thread.Sleep(Math.Max(1, SpeakMillis / Words.Length / 2));
            }
            while (!StopRequested && sw.ElapsedMilliseconds < SpeakMillis)
            {
                Thread.Sleep(5);
            }
        }

        public void Stop()
        {
            StopRequested = true;
            Interlocked.Increment(ref StopCalls);
        }

        public void SetBoundaryHandler(Action<string, int, int, float, float, bool>? handler) =>
            _handler = handler;

        /// <summary>Simulate an engine thread firing a boundary event
        /// directly into the installed handler (late/in-flight lag).</summary>
        public void FireBoundary(string word) =>
            _handler?.Invoke(word, -1, -1, 0.1f, 0.2f, true);
    }

    private readonly FakeBackend _backend = new();
    private SpeechRequestScheduler Scheduler => new(_backend);

    public void Dispose() => _backend.Speaking.Dispose();

    [Fact]
    public async Task Queue_Maintains_Order_Under_Multi_Request_Contention()
    {
        // Three requests through real gate contention (slow head);
        // exercises the acquisition loop's machinery. The not-head
        // re-wait branch is defensive against non-FIFO semaphore
        // wakeups, which .NET does not exhibit deterministically — this
        // test pins the ordering guarantee it protects.
        using var scheduler = Scheduler;
        _backend.SpeakMillis = 40;

        var tasks = Enumerable.Range(0, 3)
            .Select(i => scheduler.SubmitAsync($"r{i}", null, null, null, CancellationToken.None))
            .ToList();

        var results = await Task.WhenAll(tasks);
        Assert.All(results, r => Assert.Equal(SpeechRequestStatus.Completed, r.Status));
        lock (_backend.Spoken)
        {
            Assert.Equal(new[] { "r0", "r1", "r2" }, _backend.Spoken);
        }
        // Distinct request ids across all events/results.
        Assert.Equal(3, results.Select(r => r.RequestId).Distinct().Count());
    }

    [Fact]
    public async Task Queue_Serializes_Requests_In_Order()
    {
        using var scheduler = Scheduler;
        _backend.SpeakMillis = 50;

        var first = scheduler.SubmitAsync("one", null, null, null, CancellationToken.None);
        var second = scheduler.SubmitAsync("two", null, null, null, CancellationToken.None);

        var r1 = await first;
        var r2 = await second;

        Assert.Equal(SpeechRequestStatus.Completed, r1.Status);
        Assert.Equal(SpeechRequestStatus.Completed, r2.Status);
        lock (_backend.Spoken)
        {
            Assert.Equal(new[] { "one", "two" }, _backend.Spoken);
        }
        Assert.NotEqual(r1.RequestId, r2.RequestId);
    }

    [Fact]
    public async Task Replace_Supersedes_Active_Request()
    {
        using var scheduler = Scheduler;
        _backend.SpeakMillis = 60_000; // speaks "forever" until stopped

        var old = scheduler.SubmitAsync("old", null, null, null, CancellationToken.None);
        Assert.True(_backend.Speaking.Wait(5000), "first request must start speaking");

        // Stop unwinds the fake quickly; set the fast duration BEFORE
        // the replacement starts so the test isn't timing-coupled.
        _backend.StopRequested = true; // first unwinds via Replace's stop
        _backend.SpeakMillis = 20;

        var replacement = scheduler.SubmitAsync(
            "new", null, null,
            new SpeechRequestOptions { Dispatch = SpeechRequestDispatch.Replace },
            CancellationToken.None);

        var oldResult = await old;
        Assert.Equal(SpeechRequestStatus.Superseded, oldResult.Status);
        // The scheduler must have asked the backend to unwind the
        // superseded in-flight call (cooperative stop), not just
        // flagged it.
        Assert.True(_backend.StopCalls >= 1,
            $"Replace must call backend.Stop() (calls: {_backend.StopCalls})");

        var newResult = await replacement;
        Assert.Equal(SpeechRequestStatus.Completed, newResult.Status);
        lock (_backend.Spoken)
        {
            Assert.Equal(new[] { "old", "new" }, _backend.Spoken);
        }
    }

    [Fact]
    public async Task Cancelled_While_Queued_Returns_Result_Not_Exception()
    {
        // Contract: "the returned task completes with the request's
        // final status" — a token firing while queued behind an active
        // request must yield Status=Cancelled, not a faulted task
        // (three different cancellation shapes would otherwise exist).
        using var scheduler = Scheduler;
        using var cts = new CancellationTokenSource();
        _backend.SpeakMillis = 60_000;

        var active = scheduler.SubmitAsync("active", null, null, null, CancellationToken.None);
        Assert.True(_backend.Speaking.Wait(5000), "active must be speaking");

        var queued = scheduler.SubmitAsync("queued", null, null, null, cts.Token);
        cts.Cancel(); // fires while the queued request waits on the gate

        var queuedResult = await queued;
        Assert.Equal(SpeechRequestStatus.Cancelled, queuedResult.Status);

        // Cleanup through the scheduler's own stop path (also asserts
        // Stop() maps the active request to Stopped).
        scheduler.StopAll();
        var activeResult = await active;
        Assert.Equal(SpeechRequestStatus.Stopped, activeResult.Status);
    }

    [Fact]
    public async Task Dispose_During_InFlight_Speech_Does_Not_Fault_The_Callers_Task()
    {
        // Contract: Dispose stops everything but never converts an
        // already-computed result into an ObjectDisposedException.
        using var scheduler = Scheduler;
        _backend.SpeakMillis = 60_000;

        var task = scheduler.SubmitAsync("long", null, null, null, CancellationToken.None);
        Assert.True(_backend.Speaking.Wait(5000), "request must be speaking");

        scheduler.Dispose();
        var result = await task;
        Assert.Equal(SpeechRequestStatus.Stopped, result.Status);
    }

    [Fact]
    public async Task Stop_Cancels_Active_And_Queued_Requests()
    {
        using var scheduler = Scheduler;
        _backend.SpeakMillis = 60_000;

        var active = scheduler.SubmitAsync("active", null, null, null, CancellationToken.None);
        var queued = scheduler.SubmitAsync("queued", null, null, null, CancellationToken.None);
        Assert.True(_backend.Speaking.Wait(5000), "active request must start");

        scheduler.StopAll();

        var activeResult = await active;
        var queuedResult = await queued;
        Assert.Equal(SpeechRequestStatus.Stopped, activeResult.Status);
        Assert.Equal(SpeechRequestStatus.Cancelled, queuedResult.Status);

        lock (_backend.Spoken)
        {
            Assert.Equal(new[] { "active" }, _backend.Spoken);
        }
    }

    [Fact]
    public async Task Precancelled_Request_Never_Touches_The_Backend()
    {
        using var scheduler = Scheduler;
        using var cts = new CancellationTokenSource();
        cts.Cancel();

        var result = await scheduler.SubmitAsync(
            "never", null, null, null, cts.Token);

        Assert.Equal(SpeechRequestStatus.Cancelled, result.Status);
        lock (_backend.Spoken)
        {
            Assert.Empty(_backend.Spoken);
        }
    }

    [Fact]
    public async Task Cancellation_While_Speaking_Requests_Cooperative_Stop()
    {
        using var scheduler = Scheduler;
        using var cts = new CancellationTokenSource();
        _backend.SpeakMillis = 60_000;

        var task = scheduler.SubmitAsync("long", null, null, null, cts.Token);
        Assert.True(_backend.Speaking.Wait(5000), "request must start");
        cts.Cancel();

        var result = await task;
        Assert.Equal(SpeechRequestStatus.Cancelled, result.Status);
        Assert.True(_backend.StopRequested, "scheduler must ask the backend to stop");
    }

    [Fact]
    public async Task Timing_Events_Carry_Their_Request_Id()
    {
        using var scheduler = Scheduler;
        _backend.SpeakMillis = 30;

        var seen = new List<SpeechTimingEventArgs>();
        var result = await scheduler.SubmitAsync(
            "text",
            e => { lock (seen) seen.Add(e); },
            null, null, CancellationToken.None);

        Assert.Equal(SpeechRequestStatus.Completed, result.Status);
        lock (seen)
        {
            Assert.NotEmpty(seen);
            Assert.All(seen, e => Assert.Equal(result.RequestId, e.RequestId));
            Assert.All(seen, e => Assert.True(e.Estimated));
        }
    }

    [Fact]
    public async Task Superseded_Request_Cannot_Deliver_Callbacks_To_The_Newer_Request()
    {
        // Preview → Preview → Stop (GH #70 acceptance). The first
        // request is superseded mid-flight; its trailing boundary
        // events must be dropped, never attributed to the replacement.
        using var scheduler = Scheduler;
        _backend.SpeakMillis = 80;
        _backend.Words = new[] { "a1", "a2", "a3", "a4", "a5", "a6" };

        var firstSeen = 0;
        var secondSeen = new List<(string requestId, string word)>();

        var first = scheduler.SubmitAsync(
            "first",
            _ => Interlocked.Increment(ref firstSeen),
            null, null, CancellationToken.None);
        Assert.True(_backend.Speaking.Wait(5000));

        var second = scheduler.SubmitAsync(
            "second",
            e => { lock (secondSeen) secondSeen.Add((e.RequestId, e.Word)); },
            null,
            new SpeechRequestOptions { Dispatch = SpeechRequestDispatch.Replace },
            CancellationToken.None);

        var firstResult = await first;
        var secondResult = await second;

        Assert.Equal(SpeechRequestStatus.Superseded, firstResult.Status);
        Assert.Equal(SpeechRequestStatus.Completed, secondResult.Status);

        // Every event the second request received carried the second
        // request's id — a late event from the superseded first request
        // can never be misattributed.
        lock (secondSeen)
        {
            Assert.NotEmpty(secondSeen);
            Assert.All(secondSeen, e => Assert.Equal(secondResult.RequestId, e.requestId));
        }
    }

    [Fact]
    public async Task Late_Events_After_StopAll_Are_Dropped()
    {
        // The engine thread can lag behind Stop(): boundaries still in
        // flight must find no target and be dropped silently (GH #70:
        // a cancelled request cannot deliver callbacks).
        using var scheduler = Scheduler;
        _backend.SpeakMillis = 60_000;

        var delivered = 0;
        var task = scheduler.SubmitAsync(
            "long", _ => Interlocked.Increment(ref delivered), null, null, CancellationToken.None);
        Assert.True(_backend.Speaking.Wait(5000));

        scheduler.StopAll();
        await task;

        int beforeLate = Volatile.Read(ref delivered);
        // Simulate the engine firing a trailing boundary after Stop:
        // the scheduler's handler is still installed on the backend.
        _backend.FireBoundary("late");
        _backend.FireBoundary("later");
        Assert.Equal(beforeLate, Volatile.Read(ref delivered));
    }

    [Fact]
    public async Task Late_Events_After_Completion_Are_Dropped()
    {
        // Same invariant after natural completion: once a request is
        // finished, trailing engine events must not reach a *new*
        // request's callback with stale content.
        using var scheduler = Scheduler;
        _backend.SpeakMillis = 20;

        var secondSeen = new List<string>();
        var first = scheduler.SubmitAsync("first", null, null, null, CancellationToken.None);
        await first;
        // Fire a trailing event from the finished first utterance
        // before the second request exists: it must be dropped.
        _backend.FireBoundary("stale-from-first");

        var second = scheduler.SubmitAsync(
            "second", e => { lock (secondSeen) secondSeen.Add(e.Word); },
            null, null, CancellationToken.None);
        await second;

        lock (secondSeen)
        {
            Assert.DoesNotContain("stale-from-first", secondSeen);
        }
    }

    [Fact]
    public async Task Engine_Failure_Marks_Request_Failed_With_Error()
    {
        var backend = new ThrowingBackend();
        using var scheduler = new SpeechRequestScheduler(backend);

        var result = await scheduler.SubmitAsync("boom", null, null, null, CancellationToken.None);

        Assert.Equal(SpeechRequestStatus.Failed, result.Status);
        Assert.Contains("kaput", result.Error);
    }

    private sealed class ThrowingBackend : ISpeechBackend
    {
        public void SpeakSync(string text) => throw new InvalidOperationException("kaput");
        public void Stop() { }
        public void SetBoundaryHandler(Action<string, int, int, float, float, bool>? handler) { }
    }
}
