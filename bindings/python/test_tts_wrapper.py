"""ABI conformance + GH #68/#69/#73 parity tests for the Python binding.

Mirrors bindings/nodejs/test/abi.test.js and the .NET suites. Requires
the shared library built and located via RUST_TTS_WRAPPER_LIB (or the
loader's default search):

    cargo build --release --no-default-features --features system,cloud
    cd bindings/python
    RUST_TTS_WRAPPER_LIB=$PWD/../../target/release/librust_tts_wrapper.so \
        python -m unittest test_tts_wrapper -v
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from tts_wrapper import (
    CredentialStatus,
    EngineSetup,
    ErrorDetail,
    TTSClient,
    TtsErrorCode,
    TTSError,
    _load_lib,
)

# openai constructs offline; synthesis fails deterministically with a
# dummy key — the exact contract these tests assert.
DUMMY_CREDS = {"apiKey": "dummy-key-for-python-tests"}


def make_client(**credentials):
    return TTSClient("openai", credentials or DUMMY_CREDS)


class EngineEnumeration(unittest.TestCase):
    def test_engine_count_matches_list(self):
        engines = TTSClient.list_engines()
        self.assertGreater(len(engines), 0)
        self.assertEqual(TTSClient.engine_count(), len(engines))
        self.assertTrue(any(e.id == "openai" for e in engines))

    def test_lifecycle_double_close(self):
        c = make_client()
        c.close()
        c.close()  # idempotent

    def test_many_clients(self):
        clients = [make_client() for _ in range(8)]
        for c in clients:
            c.get_voices()
        for c in clients:
            c.close()


class ErrorSurface(unittest.TestCase):
    """GH #69: stable codes, structured detail, redaction."""

    def test_create_failure_carries_engine_not_available(self):
        with self.assertRaises(TTSError) as ctx:
            TTSClient("definitely-not-an-engine")
        self.assertEqual(ctx.exception.code, TtsErrorCode.ENGINE_NOT_AVAILABLE)
        self.assertEqual(ctx.exception.code_name, "EngineNotAvailable")
        self.assertIn("definitely-not-an-engine", str(ctx.exception))
        self.assertTrue(ctx.exception.display_message)

    def test_missing_configuration_names_the_field(self):
        # Cloud engines construct lazily; the credential check reports
        # the exact missing field with a stable code.
        with TTSClient("azure", {}) as c:
            result = c.check_credentials()
            self.assertEqual(result.status, "error")
            self.assertIsNotNone(result.error)
            self.assertEqual(result.error.code, TtsErrorCode.MISSING_CONFIGURATION)
            self.assertEqual(result.error.field_key, "subscriptionKey")
            self.assertEqual(result.error.engine_id, "azure")

    def test_closed_client_never_reads_global_state(self):
        # GH #73 review: the per-context getters must not silently fall
        # back to the process-global error slot after close.
        c = make_client()
        c.close()
        self.assertEqual(c.get_last_error_code(), 0)
        self.assertIsNone(c.get_last_error_detail())
        result = c.check_credentials()
        self.assertEqual(result.status, "error")
        self.assertEqual(result.error.message, "client closed")

    def test_static_catalog_is_unverified(self):
        # openai has no authenticated voice-list endpoint: the check
        # must answer unverified — deterministically, offline.
        with make_client() as c:
            result = c.check_credentials()
            self.assertEqual(result.status, CredentialStatus.UNVERIFIED)

    def test_synthesis_failure_code_and_redaction(self):
        creds = dict(DUMMY_CREDS)
        creds["apiKey"] = "sk-abcdef1234567890abcdef"
        creds["synthUrl"] = "http://127.0.0.1:1/v1/audio/speech"  # closed port
        with make_client(**creds) as c:
            with self.assertRaises(TTSError) as ctx:
                c.synth_to_bytes("probe")
            # Closed local port → connection refused → NetworkError on
            # every machine, no DNS, no timeouts.
            self.assertEqual(ctx.exception.code, TtsErrorCode.NETWORK_ERROR)
            self.assertNotIn("abcdef1234567890", str(ctx.exception))
            self.assertEqual(c.get_last_error_code(), TtsErrorCode.NETWORK_ERROR)
            detail = c.get_last_error_detail()
            self.assertIsInstance(detail, ErrorDetail)
            self.assertEqual(detail.code, TtsErrorCode.NETWORK_ERROR)

    def test_speak_ssml_exposed(self):
        # GH #73: speak_ssml was missing from this binding. Through the
        # closed-port engine it fails with the stable NetworkError code
        # (rate/pitch wrapping bypassed for raw SSML).
        creds = dict(DUMMY_CREDS)
        creds["synthUrl"] = "http://127.0.0.1:1/v1/audio/speech"
        with make_client(**creds) as c:
            with self.assertRaises(TTSError) as ctx:
                c.speak_ssml(
                    '<speak version="1.0" xml:lang="en-US">'
                    '<voice name="alloy">Hello</voice></speak>'
                )
            self.assertEqual(ctx.exception.code, TtsErrorCode.NETWORK_ERROR)


class SetupSchema(unittest.TestCase):
    """GH #68: typed engine setup schema."""

    def test_setups_cover_every_listed_engine(self):
        listed = {e.id for e in TTSClient.list_engines()}
        schema = {s.id for s in TTSClient.engine_setups()}
        self.assertEqual(listed, schema)

    def test_azure_renders_key_and_region(self):
        azure = TTSClient.engine_setup("azure")
        self.assertIsInstance(azure, EngineSetup)
        key = next(f for f in azure.fields if f.key == "subscriptionKey")
        self.assertEqual(key.kind, "secret")
        self.assertTrue(key.required)
        region = next(f for f in azure.fields if f.key == "region")
        self.assertEqual(region.kind, "choice")
        self.assertGreater(len(region.choices), 0)
        self.assertTrue(azure.capabilities.supports_streaming)
        self.assertFalse(azure.capabilities.static_voice_catalog)

    def test_google_either_or_group(self):
        google = TTSClient.engine_setup("google")
        self.assertIsNotNone(google)
        group = next(
            g for g in google.required_any if "apiKey" in g and "keyFilePath" in g
        )
        self.assertEqual(len(group), 2)


class Callbacks(unittest.TestCase):
    def test_registration_does_not_throw(self):
        with make_client() as c:
            c.on_boundary(lambda *args: None)
            c.on_mark(lambda *args: None)      # GH #73: new
            c.on_viseme(lambda *args: None)    # GH #73: new
            c.on_start(lambda: None)
            c.on_end(lambda: None)
            c.on_error(lambda msg: None)
            c.on_boundary(None)
            c.on_mark(None)
            c.on_viseme(None)

    def test_boundary_payload_shape(self):
        # The full 7-arg payload must be delivered (the type hint used
        # to claim 3 args — GH #73 regression guard). Assert the ctypes
        # signature's arity: word, offset, len, start, end, estimated,
        # userdata.
        c = make_client()
        captured = []
        c.on_boundary(lambda *args: captured.append(args))
        # Invoke the registered trampoline the way native code does is
        # not possible without synthesis; assert the CFUNCTYPE arg
        # count instead (7 parameters).
        import tts_wrapper as tw

        self.assertEqual(len(tw.BOUNDARY_CB._argtypes_), 7)  # type: ignore[attr-defined]
        c.close()


if __name__ == "__main__":
    unittest.main()


class ModelManagement(unittest.TestCase):
    """The #102 engine-agnostic model surface (PR #103).

    Holds under any build: with no local engine compiled in the
    catalogues are EMPTY and that is success; when sherpaonnx is
    compiled in (the local dev build), the deeper catalogue contract
    is asserted too.
    """

    def test_catalogue_and_voice_catalog_are_lists(self):
        models = TTSClient.local_models()
        voices = TTSClient.installed_voices()
        self.assertIsInstance(models, list)
        self.assertIsInstance(voices, list)
        for m in models:
            self.assertTrue(m.engine)
            self.assertTrue(m.id)
            self.assertTrue(m.availability)
        for v in voices:
            self.assertTrue(v.engine)
            self.assertTrue(v.model)
            self.assertTrue(v.voice)

    def test_search_returns_list_and_unknown_engine_dir_raises(self):
        self.assertIsInstance(TTSClient.search_models("zzz-no-such-model"), list)
        with self.assertRaises(TTSError):
            TTSClient.models_dir("no-such-engine")

    def test_unknown_model_raises_model_not_found(self):
        with self.assertRaises(TTSError):
            TTSClient.local_model(None, "zzz-no-such-model")

    def test_install_unknown_model_raises(self):
        with self.assertRaises(TTSError):
            TTSClient.install_model("sherpaonnx", "zzz-no-such-model")

    def _sherpa_present(self) -> bool:
        return any(m.engine == "sherpaonnx" for m in TTSClient.local_models())

    def test_sherpa_catalogue_contract_when_compiled_in(self):
        if not self._sherpa_present():
            self.skipTest("sherpaonnx not compiled into this build")
        sherpa = TTSClient.local_models("sherpaonnx")
        self.assertTrue(sherpa)
        amy = TTSClient.local_model("sherpaonnx", "piper-en_US-amy-low")
        self.assertEqual(amy.id, "piper-en_US-amy-low")
        self.assertIn(
            amy.availability,
            ("needs_download", "ready", "needs_bundle", "unverified"),
        )
        # search finds by id (a unique needle — order is per-process)
        hits = TTSClient.search_models("piper-en_US-amy-low")
        self.assertTrue(any(h.id == "piper-en_US-amy-low" for h in hits))
        # the models dir ends with the engine layout segment
        self.assertTrue(TTSClient.models_dir("sherpaonnx").endswith("sherpaonnx"))
        # floravox-student rows never appear in the sherpa catalogue
        self.assertFalse(
            any(m.id.startswith("willwade-student-") for m in sherpa)
        )
        # floravox rows (when compiled in) are badged experimental
        fv = [m for m in TTSClient.local_models() if m.engine == "floravox"]
        for m in fv:
            self.assertTrue(m.experimental)
