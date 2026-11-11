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
system loader path, or honours the `RUST_TTS_WRAPPER_LIB` environment
variable for an explicit path:

```bash
cargo build --release --no-default-features --features system,cloud  # Linux
export RUST_TTS_WRAPPER_LIB=target/release/librust_tts_wrapper.so
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

## Errors, credentials and the setup schema (GH #68/#69/#73)

```python
from tts_wrapper import TtsErrorCode, TTSError

# 1) Structured errors: stable codes + secret-redacted messages.
try:
    client.synth_to_bytes("hello")
except TTSError as e:
    e.code           # TtsErrorCode.NETWORK_ERROR (4)
    e.field_key      # offending settings row ("" when n/a)
    e.display_message  # safe, display-ready hint
    e.engine_id

client.get_last_error_code()     # stable u32 after any failure
client.get_last_error_detail()   # ErrorDetail (message redacted)

# 2) Tri-state credential check: static catalogs answer "unverified".
result = client.check_credentials()
result.status   # "verified" | "rejected" | "unverified" | "error"
result.error    # ErrorDetail when status == "error"
                 # (e.g. MissingConfiguration with field_key)

# 3) Typed engine setup schema — render settings UIs without a
#    provider switch (kinds: secret / filePath / directoryPath / url /
#    choice / number / text; either/or groups; capabilities).
for setup in TTSClient.engine_setups():
    for f in setup.fields:
        print(setup.id, f.key, f.kind, f.required)
azure = TTSClient.engine_setup("azure")
```

## Raw SSML, marks and visemes (GH #73)

```python
client.speak_ssml('<speak version="1.0" xml:lang="en-US">'
                  '<voice name="alloy">Hello</voice></speak>')

# word, byte_offset, byte_len, start_s, end_s, estimated
client.on_boundary(lambda w, off, ln, s, e, est: ...)
# name, char_offset, start_s, end_s — SSML <mark> events
client.on_mark(lambda name, off, s, e: ...)
# viseme_id, offset_s — lip-sync
client.on_viseme(lambda vid, off: ...)
```

## Tests

```bash
cargo build --release --no-default-features --features system,cloud
cd bindings/python
RUST_TTS_WRAPPER_LIB=$PWD/../../target/release/librust_tts_wrapper.so \
  python -m unittest test_tts_wrapper -v
```
