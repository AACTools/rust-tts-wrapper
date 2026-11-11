# Python usage

Source: [`bindings/python/tts_wrapper.py`](../bindings/python/tts_wrapper.py) — a
single-file ctypes wrapper over the C ABI. Copy it into your project or
vendor the directory; no PyPI package yet.

```python
from tts_wrapper import TTSClient

client = TTSClient("openai", {"apiKey": "your-key"})

# Streaming audio chunks (PCM16 mono) as they arrive
client.on_audio(lambda chunk: play(chunk))

# Word timings: word, byte_offset, byte_len, start_s, end_s, estimated
client.on_boundary(lambda w, off, ln, s, e, est:
    print(f"{w}: {s:.3f}-{e:.3f}{'~' if est else ''}"))

client.set_voice("alloy")
client.set_rate(1.1)
client.speak_sync("The quick brown fox")
client.stop()
```

## Loading the native library

The wrapper looks for `rust_tts_wrapper` (`.dll`/`.so`/`.dylib`) on the
system loader path, or honours the `TTS_WRAPPER_LIB` environment
variable for an explicit path:

```bash
cargo build --release --no-default-features --features system,cloud  # Linux
export TTS_WRAPPER_LIB=target/release/librust_tts_wrapper.so
python demo.py
```

## Engine selection & credentials

```python
engines = client.list_engines()          # ids compiled into this build
voices  = client.get_voices()            # per-engine voice list

client = TTSClient("azure", {"subscriptionKey": "...", "region": "eastus"})
```

Credentials are plain dicts serialised to the JSON the Rust factory
parses (canonical keys — see `tts_get_engine_setups_json` or the
[engines doc](engines.md) for the full mapping).
