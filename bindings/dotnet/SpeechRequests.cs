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
        public string? Error { get; set; }
    }

    private readonly ISpeechBackend _backend;
    private readonly SemaphoreSlim _gate = new(1, 1);
    private readonly object _sync = new();
    private readonly List<Request> _queued = new();
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

        return RunRequestAsync(request, text, prepare, ct);
    }

    private async Task<SpeechRequestResult> RunRequestAsync(
        Request request,
        string text,
        Action? prepare,
        CancellationToken ct)
    {
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            lock (_sync)
            {
                if (request.State == SpeechRequestStatus.Superseded ||
                    request.State == SpeechRequestStatus.Cancelled)
                {
                    // Stopped/removed while queued.
                    request.Tcs.TrySetResult(MakeResult(request));
                    return request.Tcs.Task.Result;
                }
                _queued.Remove(request);
                request.State = SpeechRequestStatus.Speaking;
                _active = request;
            }

            prepare?.Invoke();

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
                        request.Error = ex.Message;
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
                request.Tcs.TrySetResult(MakeResult(request));
                return request.Tcs.Task.Result;
            }
        }
        finally
        {
            _gate.Release();
        }
    }

    /// <summary>
    /// Stop everything (the client's <c>Stop()</c>): the active request
    /// unwinds cooperatively and completes
    /// <see cref="SpeechRequestStatus.Stopped"/>; queued requests never
    /// start and complete <see cref="SpeechRequestStatus.Cancelled"/>.
    /// </summary>
    public void StopAll()
    {
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
            }
            _active = null;
        }
        // Stop the in-flight native synthesis outside the lock; the
        // speak thread then unwinds and finalizes the record.
        if (activeToStop is not null)
        {
            _backend.Stop();
        }
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
            if (target is null || target.State != SpeechRequestStatus.Speaking)
            {
                return;
            }
        }
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

    private static SpeechRequestResult MakeResult(Request r) => new()
    {
        RequestId = r.Id,
        Status = r.State,
        Error = r.Error,
    };

    public void Dispose()
    {
        lock (_sync)
        {
            if (_disposed) return;
            _disposed = true;
        }
        StopAll();
        _backend.SetBoundaryHandler(null);
        _gate.Dispose();
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
