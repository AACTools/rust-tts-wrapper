# Python bindings for rust-tts-wrapper

Two layers are provided:

1. **`TTSClient`** — a thin wrapper around the C ABI via `ctypes`. Returns
   plain Python types. Use this when you want full control and no extra
   dependencies.
2. **`RustTtsClient`** — a subclass of `tts_wrapper.tts.TTSClient` (the
   pure-Python [`tts-wrapper`](https://pypi.org/project/tts-wrapper/) package
   on PyPI) so projects already using that surface can swap backends with one
   import change.

## Install

Drop the compiled native library (`librust_tts_wrapper.so` / `.dylib` /
`rust_tts_wrapper.dll`) next to this file. To point at a different location,
set the `RUST_TTS_WRAPPER_LIB` environment variable.

## Low-level usage (`TTSClient`)

```python
from rust_tts_wrapper import TTSClient

with TTSClient("azure", {"subscriptionKey": os.environ["AZURE_KEY"], "region": "uksouth"}) as client:
    client.set_voice("en-US-AriaNeural")
    client.set_rate(1.1)
    audio = client.synth_to_bytes("Hello world")

    # Streaming audio + word boundaries
    client.on_audio(lambda chunk: print(f"got {len(chunk)} bytes", file=sys.stderr))
    client.on_boundary(lambda word, start, end: print(f"[{start:.2f}-{end:.2f}] {word}"))
    client.speak_sync("The quick brown fox.")
```

## Drop-in for `tts-wrapper` (`RustTtsClient`)

Install the pure-Python package: `pip install tts-wrapper`. Then swap the
backend with one import:

```python
# Before
from tts_wrapper import WatsonTTSClient
client = WatsonTTSClient(credentials=...)

# After — same surface, Rust backend underneath
from rust_tts_wrapper import RustTtsClient
client = RustTtsClient("watson", credentials=...)
```

`RustTtsClient` implements `get_voices`, `synth_to_bytes`, `speak`,
`speak_streamed`, `stop`, `pause`, `resume`, and `close` by delegating to
the native engine.

## Discovery

```python
from rust_tts_wrapper import TTSClient

engines = TTSClient.list_engines()  # List[EngineInfo]
print([e.id for e in engines])
print(TTSClient.engine_count())
```

## Notes

- The `list_engines()` module-level helper still exists for backwards
  compatibility, but it returns only the count. Prefer
  `TTSClient.list_engines()` which returns the full list of engine
  descriptors.
- The native library is loaded lazily on first use; loading errors raise
  `OSError` from `ctypes.CDLL`.
