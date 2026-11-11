using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;

namespace RustTtsWrapper;

/// <summary>How a new speech request interacts with outstanding ones (GH #70).</summary>
public enum SpeechRequestDispatch
{
    /// <summary>Serialize: the request waits for any active/queued
    /// speech to finish (settings previews that must not overlap).</summary>
    Queue,
    /// <summary>Supersede: outstanding requests (active and queued) are
    /// stopped and completed as <see cref="SpeechRequestStatus.Superseded"/>;
    /// the new request starts immediately after the active one unwinds.</summary>
    Replace,
}

/// <summary>Lifecycle state of a speech request (GH #70).</summary>
public enum SpeechRequestStatus
{
    /// <summary>Waiting for an earlier request to finish.</summary>
    Queued,
    /// <summary>Currently synthesizing/speaking.</summary>
    Speaking,
    /// <summary>Finished normally.</summary>
    Completed,
    /// <summary>Aborted by <c>Stop()</c>.</summary>
    Stopped,
    /// <summary>Displaced by a newer request (<see cref="SpeechRequestDispatch.Replace"/>).</summary>
    Superseded,
    /// <summary>Cancelled via its <see cref="CancellationToken"/> before
    /// delivery of its results.</summary>
    Cancelled,
    /// <summary>The engine failed; see <see cref="SpeechRequestResult.Error"/>.</summary>
    Failed,
}

/// <summary>Options for <c>SpeakRequestAsync</c> (GH #70).</summary>
public sealed class SpeechRequestOptions
{
    /// <summary>Queue or Replace semantics. Default: Queue.</summary>
    public SpeechRequestDispatch Dispatch { get; init; } = SpeechRequestDispatch.Queue;
}

/// <summary>
/// Word-timing event carrying the owning request id and the full
/// boundary payload (GH #70). Offsets follow the native contract:
/// UTF-8 byte offset/length into the spoken text, -1 when unavailable
/// (SAPI reports UTF-16 code units — see the C ABI docs).
/// </summary>
public sealed class SpeechTimingEventArgs : EventArgs
{
    public required string RequestId { get; init; }
    public required string Word { get; init; }
    /// <summary>Byte offset into the spoken text, -1 when unknown.</summary>
    public int CharOffset { get; init; }
    /// <summary>Byte length of the word, -1 when unknown.</summary>
    public int CharLen { get; init; }
    /// <summary>Start time in seconds.</summary>
    public float StartTime { get; init; }
    /// <summary>End time in seconds.</summary>
    public float EndTime { get; init; }
    /// <summary>True when timings are proportional estimates.</summary>
    public bool Estimated { get; init; }
}

/// <summary>Final outcome of a speech request (GH #70).</summary>
public sealed class SpeechRequestResult
{
    public required string RequestId { get; init; }
    public required SpeechRequestStatus Status { get; init; }
    /// <summary>Error message when <see cref="SpeechRequestStatus.Failed"/>.</summary>
    public string? Error { get; init; }
    /// <summary>The engine exception for a Failed request, so legacy
    /// wrappers (and callers wanting the structured
    /// <see cref="TtsException"/> detail) can rethrow the original.</summary>
    public Exception? ErrorException { get; init; }
}

/// <summary>
/// The speech backend the scheduler drives. Extracted so the request
/// semantics (queue/replace/stop/cancellation, callback isolation) are
/// unit-testable without a native library; <see cref="TtsClient"/>
/// implements it via a small adapter.
/// </summary>
internal interface ISpeechBackend
{
    /// <summary>Speak synchronously; returns when speech completes or fails.</summary>
    void SpeakSync(string text);

    /// <summary>Request cooperative stop of the active SpeakSync.</summary>
    void Stop();

    /// <summary>
    /// Install (or clear, with null) the single boundary handler the
    /// backend delivers its word-timing events to. The scheduler owns
    /// this registration; callers must not register their own.
    /// </summary>
    void SetBoundaryHandler(Action<string, int, int, float, float, bool>? handler);
}

/// <summary>
/// Per-client speech request scheduler (GH #70).
///
/// Semantics (documented contract):
///  - Every request gets a unique id, present in its timing events and
///    final result, so a UI can correlate previews with completions.
///  - <see cref="SpeechRequestDispatch.Queue"/>: FIFO — one request at
///    a time, in submission order.
///  - <see cref="SpeechRequestDispatch.Replace"/>: outstanding requests
///    (active + queued) are stopped and completed
///    <see cref="SpeechRequestStatus.Superseded"/>; the new request
///    starts as soon as the active call unwinds.
///  - <c>Stop()</c> (<see cref="StopAll"/>): the active request is
///    stopped cooperatively (native <c>tts_stop</c>) and completes
///    <see cref="SpeechRequestStatus.Stopped"/>; queued requests never
///    start and complete <see cref="SpeechRequestStatus.Cancelled"/>.
///  - Cancellation: a token cancelled before start never touches the
///    backend. Once a request is speaking, cancellation requests a
///    cooperative stop — engines honour <c>tts_stop</c> at their next
///    checkpoint; a cancelled or superseded request's late boundary
///    events are dropped and are never delivered to a newer request.
/// </summary>
internal sealed class SpeechRequestScheduler : IDisposable
{
    private sealed class Request
    {
        public required string Id { get; init; }
        public SpeechRequestStatus State { get; set; } = SpeechRequestStatus.Queued;
        public Action<SpeechTimingEventArgs>? Timing { get; init; }
        public TaskCompletionSource<SpeechRequestResult> Tcs { get; } =
            new(TaskCreationOptions.RunContinuationsAsynchronously);
        /// <summary>The engine exception for a Failed request (kept so
        /// legacy wrappers can rethrow the original structured error).</summary>
        public Exception? ErrorException { get; set; }
        public string? Error => ErrorException?.Message;
    }

    private readonly ISpeechBackend _backend;
    private readonly SemaphoreSlim _gate = new(1, 1);
    private readonly object _sync = new();
    private readonly List<Request> _queued = new();
    /// <summary>Requests superseded while speaking: their native call
    /// is still unwinding (stopped, but the runner hasn't finalized
    /// yet) — Dispose drains these too (GH #70 loop-4).</summary>
    private readonly List<Request> _unwinding = new();
    private Request? _active;
    private bool _disposed;

    public SpeechRequestScheduler(ISpeechBackend backend)
    {
        _backend = backend;
        // One persistent boundary registration: the backend may deliver
        // events from any in-flight speak; the handler routes them to
        // the current active request, dropping late events from
        // stopped/superseded ones (GH #70 acceptance: "a cancelled or
        // superseded request cannot deliver callbacks to a newer
        // request").
        _backend.SetBoundaryHandler(OnBoundary);
    }

    /// <summary>Submit a speech request. The returned task completes
    /// with the request's final status.</summary>
    public Task<SpeechRequestResult> SubmitAsync(
        string text,
        Action<SpeechTimingEventArgs>? timing,
        Action? prepare,
        SpeechRequestOptions? options,
        CancellationToken ct)
    {
        ArgumentNullException.ThrowIfNull(text);

        options ??= new SpeechRequestOptions();
        var request = new Request { Id = Guid.NewGuid().ToString("N"), Timing = timing };
        bool supersededInFlight = false;

        lock (_sync)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);

            if (options.Dispatch == SpeechRequestDispatch.Replace)
            {
                // Supersede the active request and anything queued.
                if (_active is { } active && active.State == SpeechRequestStatus.Speaking)
                {
                    active.State = SpeechRequestStatus.Superseded;
                    supersededInFlight = true;
                    // Still unwinding inside its native call — Dispose
                    // must drain it even though it is no longer _active.
                    _unwinding.Add(active);
                }
                foreach (var q in _queued)
                {
                    q.State = SpeechRequestStatus.Superseded;
                    q.Tcs.TrySetResult(MakeResult(q));
                }
                _queued.Clear();
                _active = null; // no longer eligible for new events
            }

            if (ct.IsCancellationRequested)
            {
                request.State = SpeechRequestStatus.Cancelled;
                request.Tcs.SetResult(MakeResult(request));
                return request.Tcs.Task;
            }

            _queued.Add(request);
        }

        if (supersededInFlight)
        {
            // Ask the backend to unwind the superseded native call now;
            // it releases the gate when it returns, letting the new
            // request start. (Harmless no-op if it already finished.)
            _backend.Stop();
        }

        // The PUBLIC task is the request's own completion source: it is
        // completed with a result on every path (including
        // stop/supersede/cancel from other threads), so machinery
        // faults in the runner can never leak to the caller. The runner
        // itself is watched by a catch-all.
        _ = RunRequestAsync(request, text, prepare, ct);
        return request.Tcs.Task;
    }

    private async Task RunRequestAsync(
        Request request,
        string text,
        Action? prepare,
        CancellationToken ct)
    {
        bool holdsGate = false;
        try
        {
            // FIFO head-of-queue acquisition loop: SemaphoreSlim does
            // not guarantee FIFO wakeups under contention, so after
            // acquiring the gate verify this request is the oldest
            // queued one; otherwise yield the gate and re-wait. Gate
            // ownership is tracked explicitly so a cancellation landing
            // on a re-wait can never double-release.
            while (true)
            {
                try
                {
                    await _gate.WaitAsync(ct).ConfigureAwait(false);
                    holdsGate = true;
                }
                catch (OperationCanceledException)
                {
                    // Cancelled while waiting: complete as Cancelled
                    // with a result object — never fault the caller's
                    // task ("completes with the request's final status"
                    // holds for every cancellation interleaving).
                    lock (_sync)
                    {
                        request.State = SpeechRequestStatus.Cancelled;
                        _queued.Remove(request);
                        request.Tcs.TrySetResult(MakeResult(request));
                    }
                    return;
                }

                lock (_sync)
                {
                    if (request.State == SpeechRequestStatus.Superseded ||
                        request.State == SpeechRequestStatus.Cancelled ||
                        request.State == SpeechRequestStatus.Stopped)
                    {
                        // Terminal state landed while queued.
                        _queued.Remove(request);
                        request.Tcs.TrySetResult(MakeResult(request));
                        return;
                    }
                    if (_queued.Count == 0 || ReferenceEquals(_queued[0], request))
                    {
                        _queued.Remove(request);
                        request.State = SpeechRequestStatus.Speaking;
                        _active = request;
                        break; // gate held, we are the head
                    }
                    // Not the head: another waiter jumped the queue.
                    // Yield the gate and re-wait.
                }
                holdsGate = false;
                _gate.Release();
                await Task.Yield();
            }

            // Prepare (apply options) under the gate; a failure here is
            // a Failed request, not a faulted task.
            try
            {
                prepare?.Invoke();
            }
            catch (Exception ex)
            {
                lock (_sync)
                {
                    request.ErrorException = ex;
                    request.State = SpeechRequestStatus.Failed;
                    if (ReferenceEquals(_active, request))
                    {
                        _active = null;
                    }
                    request.Tcs.TrySetResult(MakeResult(request));
                }
                return;
            }

            // Final pre-flight check: a Stop/supersede that landed between
            // the state transition and the native call is honoured
            // without speaking at all.
            lock (_sync)
            {
                if (request.State != SpeechRequestStatus.Speaking)
                {
                    if (ReferenceEquals(_active, request))
                    {
                        _active = null;
                    }
                    request.Tcs.TrySetResult(MakeResult(request));
                    return;
                }
            }

            // Cancellation while speaking: ask the engine to stop
            // cooperatively; backends that cannot abort an in-flight
            // call finish it and the request completes Cancelled anyway.
            using CancellationTokenRegistration ctr = ct.Register(
                static s => ((ISpeechBackend)s!).Stop(),
                _backend,
                useSynchronizationContext: false);

            try
            {
                await Task.Run(
                    () => _backend.SpeakSync(text),
                    CancellationToken.None).ConfigureAwait(false);
            }
            catch (Exception ex)
            {
                lock (_sync)
                {
                    if (request.State == SpeechRequestStatus.Speaking)
                    {
                        request.ErrorException = ex;
                        request.State = ct.IsCancellationRequested
                            ? SpeechRequestStatus.Cancelled
                            : SpeechRequestStatus.Failed;
                    }
                }
            }

            lock (_sync)
            {
                if (request.State == SpeechRequestStatus.Speaking)
                {
                    // Normal return: Completed — unless the token fired
                    // during the call and requested a cooperative stop
                    // (observed cancellation reads Cancelled).
                    request.State = ct.IsCancellationRequested
                        ? SpeechRequestStatus.Cancelled
                        : SpeechRequestStatus.Completed;
                }
                if (ReferenceEquals(_active, request))
                {
                    _active = null;
                }
                _unwinding.Remove(request);
                request.Tcs.TrySetResult(MakeResult(request));
            }
        }
        catch (Exception ex)
        {
            // Runner-machinery safety net: the public task (the TCS)
            // must always complete with a result, never fault.
            lock (_sync)
            {
                if (request.State is SpeechRequestStatus.Queued or SpeechRequestStatus.Speaking)
                {
                    request.ErrorException = ex;
                    request.State = SpeechRequestStatus.Failed;
                    if (ReferenceEquals(_active, request))
                    {
                        _active = null;
                    }
                }
                _queued.Remove(request);
                _unwinding.Remove(request);
                request.Tcs.TrySetResult(MakeResult(request));
            }
        }
        finally
        {
            if (holdsGate)
            {
                _gate.Release();
            }
        }
    }

    /// <summary>
    /// Stop everything (the client's <c>Stop()</c>): the active request
    /// unwinds cooperatively and completes
    /// <see cref="SpeechRequestStatus.Stopped"/>; queued requests never
    /// start and complete <see cref="SpeechRequestStatus.Cancelled"/>.
    /// </summary>
    public void StopAll() => _ = StopAllCollecting();

    /// <summary>StopAll that also returns the in-flight request's
    /// completion task (for Dispose's drain — avoids the snapshot/
    /// stop race where a request activates between snapshotting and
    /// stopping).</summary>
    private List<Task> StopAllCollecting()
    {
        var stoppedInFlight = new List<Task>();
        Request? activeToStop;
        lock (_sync)
        {
            foreach (var q in _queued)
            {
                q.State = SpeechRequestStatus.Cancelled;
                q.Tcs.TrySetResult(MakeResult(q));
            }
            _queued.Clear();

            activeToStop = _active;
            if (_active is { State: SpeechRequestStatus.Speaking })
            {
                _active.State = SpeechRequestStatus.Stopped;
                stoppedInFlight.Add(_active.Tcs.Task);
            }
            _active = null;
            // Replace-superseded speaks still unwinding natively.
            foreach (var u in _unwinding)
            {
                stoppedInFlight.Add(u.Tcs.Task);
            }
        }
        // Stop the in-flight native synthesis outside the lock; the
        // speak thread then unwinds and finalizes the record.
        if (activeToStop is not null)
        {
            _backend.Stop();
        }
        return stoppedInFlight;
    }

    private void OnBoundary(string word, int charOffset, int charLen, float start, float end, bool estimated)
    {
        Request? target;
        lock (_sync)
        {
            target = _active;
            // Drop late events: once a request is stopped/superseded it
            // is removed from _active, so its trailing events find no
            // target and a newer request can never receive them.
            //
            // ASSUMPTION: backends deliver boundary events synchronously
            // within their SpeakSync call (true for every native engine:
            // callbacks fire from inside speak()). An engine emitting
            // from another thread AFTER its SpeakSync returned could in
            // principle race a newer request's window — native
            // request-id tagging is the long-term fix.
            if (target is null || target.State != SpeechRequestStatus.Speaking)
            {
                return;
            }
        }
        try
        {
            target.Timing?.Invoke(new SpeechTimingEventArgs
            {
                RequestId = target.Id,
                Word = word,
                CharOffset = charOffset,
                CharLen = charLen,
                StartTime = start,
                EndTime = end,
                Estimated = estimated,
            });
        }
        catch
        {
            // User callback exceptions must not propagate through the
            // reverse P/Invoke boundary (process-fatal in .NET).
        }
    }

    private static SpeechRequestResult MakeResult(Request r) => new()
    {
        RequestId = r.Id,
        Status = r.State,
        Error = r.Error,
        ErrorException = r.ErrorException,
    };

    /// <summary>
    /// Stop everything and drain: the in-flight native speak unwinds
    /// (bounded) before this returns, so owners can free the native
    /// engine immediately after. Must NOT be called from a timing
    /// callback (it runs on the speak thread — the drain would block
    /// the only thread able to complete it for the full timeout).
    /// </summary>
    public void Dispose()
    {
        lock (_sync)
        {
            if (_disposed) return;
            _disposed = true;
        }
        // StopAllCollecting captures the in-flight task(s) under the
        // SAME lock that stops them — no snapshot/stop window.
        var stoppedInFlight = StopAllCollecting();
        _backend.SetBoundaryHandler(null);
        foreach (var task in stoppedInFlight)
        {
            // Bounded so a wedged engine can't hang Dispose forever.
            try
            {
                task.Wait(TimeSpan.FromSeconds(10));
            }
            catch (AggregateException)
            {
                // TCS tasks complete with results, never fault.
            }
        }
        // NOTE: the gate is deliberately NOT disposed — an in-flight
        // runner still holds it and its finally-block would throw
        // ObjectDisposedException after the result was already
        // delivered (faulting internal machinery). The semaphore is
        // GC-owned; nothing leaks.
    }
}

/// <summary>
/// Adapter implementing <see cref="ISpeechBackend"/> over the native
/// <see cref="TtsClient"/> — the seam that keeps the scheduler testable.
/// </summary>
internal sealed class TtsClientBackend : ISpeechBackend
{
    private readonly TtsClient _client;

    public TtsClientBackend(TtsClient client)
    {
        _client = client;
    }

    public void SpeakSync(string text) => _client.SpeakSync(text);
    public void Stop() => _client.Stop();

    public void SetBoundaryHandler(Action<string, int, int, float, float, bool>? handler) =>
        _client.SetOnBoundary(handler is null
            ? null
            : (word, charOffset, charLen, start, end, estimated) =>
                handler(word, charOffset, charLen, start, end, estimated));
}
