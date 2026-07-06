"""Python bindings for rust-tts-wrapper via ctypes.

Two layers are provided:

* :class:`TTSClient` — a thin wrapper around the C ABI. Returns plain Python
  types. Use this when you want full control and no extra dependencies.
* :class:`RustTtsClient` — a subclass of ``tts_wrapper.tts.TTSClient`` (the
  pure-Python ``tts-wrapper`` package on PyPI) so projects that already use
  that surface can swap backends with one import change.

The native library is loaded from the same directory as this file. Set
``RUST_TTS_WRAPPER_LIB`` to point at a different path if needed.
"""

from __future__ import annotations

import ctypes
import json
import os
import platform
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Dict, List, Optional

# ---------------------------------------------------------------------------
# ctypes setup
# ---------------------------------------------------------------------------

_lib = None

AUDIO_CB = ctypes.CFUNCTYPE(
    None, ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t, ctypes.c_void_p
)
BOUNDARY_CB = ctypes.CFUNCTYPE(
    None, ctypes.c_char_p, ctypes.c_float, ctypes.c_float, ctypes.c_void_p
)
VOID_CB = ctypes.CFUNCTYPE(None, ctypes.c_void_p)
ERROR_CB = ctypes.CFUNCTYPE(None, ctypes.c_char_p, ctypes.c_void_p)


class _TtsVoiceC(ctypes.Structure):
    """Mirror of `tts_voice` in include/tts_wrapper.h."""

    _fields_ = [
        ("id", ctypes.c_char_p),
        ("name", ctypes.c_char_p),
        ("language", ctypes.c_char_p),
        ("gender", ctypes.c_char_p),
        ("engine", ctypes.c_char_p),
    ]


class _TtsEngineInfoC(ctypes.Structure):
    """Mirror of `tts_engine_info` in include/tts_wrapper.h."""

    _fields_ = [
        ("id", ctypes.c_char_p),
        ("name", ctypes.c_char_p),
        ("needs_credentials", ctypes.c_bool),
        ("credential_keys_json", ctypes.c_char_p),
    ]


def _default_lib_path() -> Path:
    here = Path(__file__).parent
    system = platform.system()
    if system == "Linux":
        return here / "librust_tts_wrapper.so"
    if system == "Darwin":
        return here / "librust_tts_wrapper.dylib"
    return here / "rust_tts_wrapper.dll"


def _load_lib():
    """Load and configure the native library (cached on first call)."""
    global _lib
    if _lib is not None:
        return _lib

    lib_path = Path(os.environ.get("RUST_TTS_WRAPPER_LIB", _default_lib_path()))
    _lib = ctypes.CDLL(str(lib_path))

    _lib.tts_create.restype = ctypes.c_void_p
    _lib.tts_create.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
    _lib.tts_destroy.restype = None
    _lib.tts_destroy.argtypes = [ctypes.c_void_p]
    _lib.tts_speak.restype = ctypes.c_int32
    _lib.tts_speak.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    _lib.tts_speak_sync.restype = ctypes.c_int32
    _lib.tts_speak_sync.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    _lib.tts_stop.restype = None
    _lib.tts_stop.argtypes = [ctypes.c_void_p]
    _lib.tts_pause.restype = None
    _lib.tts_pause.argtypes = [ctypes.c_void_p]
    _lib.tts_resume.restype = None
    _lib.tts_resume.argtypes = [ctypes.c_void_p]
    _lib.tts_synth_to_bytes.restype = ctypes.c_int32
    _lib.tts_synth_to_bytes.argtypes = [
        ctypes.c_void_p,
        ctypes.c_char_p,
        ctypes.POINTER(ctypes.POINTER(ctypes.c_uint8)),
        ctypes.POINTER(ctypes.c_size_t),
    ]
    _lib.tts_free_bytes.restype = None
    _lib.tts_free_bytes.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t]
    _lib.tts_set_voice.restype = None
    _lib.tts_set_voice.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    _lib.tts_set_rate.restype = None
    _lib.tts_set_rate.argtypes = [ctypes.c_void_p, ctypes.c_float]
    _lib.tts_set_pitch.restype = None
    _lib.tts_set_pitch.argtypes = [ctypes.c_void_p, ctypes.c_float]
    _lib.tts_set_volume.restype = None
    _lib.tts_set_volume.argtypes = [ctypes.c_void_p, ctypes.c_float]
    _lib.tts_set_on_audio.restype = None
    _lib.tts_set_on_audio.argtypes = [ctypes.c_void_p, AUDIO_CB, ctypes.c_void_p]
    _lib.tts_set_on_boundary.restype = None
    _lib.tts_set_on_boundary.argtypes = [ctypes.c_void_p, BOUNDARY_CB, ctypes.c_void_p]
    _lib.tts_set_on_start.restype = None
    _lib.tts_set_on_start.argtypes = [ctypes.c_void_p, VOID_CB, ctypes.c_void_p]
    _lib.tts_set_on_end.restype = None
    _lib.tts_set_on_end.argtypes = [ctypes.c_void_p, VOID_CB, ctypes.c_void_p]
    _lib.tts_set_on_error.restype = None
    _lib.tts_set_on_error.argtypes = [ctypes.c_void_p, ERROR_CB, ctypes.c_void_p]
    _lib.tts_get_voices.restype = ctypes.c_int32
    _lib.tts_get_voices.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.POINTER(_TtsVoiceC)),
        ctypes.POINTER(ctypes.c_int32),
    ]
    _lib.tts_free_voices.restype = None
    _lib.tts_free_voices.argtypes = [ctypes.POINTER(_TtsVoiceC), ctypes.c_int32]
    _lib.tts_get_engine_count.restype = ctypes.c_int32
    _lib.tts_get_engines.restype = ctypes.c_int32
    _lib.tts_get_engines.argtypes = [
        ctypes.POINTER(ctypes.POINTER(_TtsEngineInfoC)),
        ctypes.POINTER(ctypes.c_int32),
    ]
    _lib.tts_free_engines.restype = None
    _lib.tts_free_engines.argtypes = [ctypes.POINTER(_TtsEngineInfoC), ctypes.c_int32]
    _lib.tts_get_last_error.restype = ctypes.c_char_p
    _lib.tts_get_last_error.argtypes = [ctypes.c_void_p]
    return _lib


# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------


@dataclass
class Voice:
    """A TTS voice with metadata."""

    id: str
    name: str
    language: str
    gender: str = ""
    engine: str = ""

    def __repr__(self) -> str:
        return f"Voice(id={self.id!r}, name={self.name!r}, engine={self.engine!r})"


@dataclass
class EngineInfo:
    """A compiled-in engine descriptor."""

    id: str
    name: str
    needs_credentials: bool
    credential_keys: List[str] = field(default_factory=list)

    def __repr__(self) -> str:
        return f"EngineInfo(id={self.id!r}, name={self.name!r})"


class TTSError(RuntimeError):
    """Raised when a TTS operation fails. Message comes from tts_get_last_error."""


# ---------------------------------------------------------------------------
# Client
# ---------------------------------------------------------------------------


class TTSClient:
    """Low-level Python client wrapping the Rust C ABI."""

    def __init__(self, engine_id: str = "system", credentials: Optional[dict] = None):
        self._lib = _load_lib()
        creds_json = json.dumps(credentials or {})
        self._ctx = self._lib.tts_create(engine_id.encode(), creds_json.encode())
        if not self._ctx:
            raise TTSError(f"Failed to create engine '{engine_id}': {self.get_last_error()}")
        # Keep strong refs to the ctypes callback wrappers so the GC doesn't
        # collect the function pointer while native code still holds it.
        self._audio_cb_ref: Optional[AUDIO_CB] = None
        self._boundary_cb_ref: Optional[BOUNDARY_CB] = None
        self._start_cb_ref: Optional[VOID_CB] = None
        self._end_cb_ref: Optional[VOID_CB] = None
        self._error_cb_ref: Optional[ERROR_CB] = None

    # --- context manager -----------------------------------------------

    def __enter__(self) -> "TTSClient":
        return self

    def __exit__(self, *_exc) -> None:
        self.close()

    def __del__(self):
        try:
            self.close()
        except Exception:
            pass

    def close(self) -> None:
        ctx = getattr(self, "_ctx", None)
        if ctx:
            self._lib.tts_destroy(ctx)
            self._ctx = None

    # --- synthesis -----------------------------------------------------

    def speak(self, text: str) -> None:
        if self._lib.tts_speak(self._ctx, text.encode()) != 0:
            raise TTSError(self.get_last_error() or "speak failed")

    def speak_sync(self, text: str) -> None:
        if self._lib.tts_speak_sync(self._ctx, text.encode()) != 0:
            raise TTSError(self.get_last_error() or "speak_sync failed")

    def synth_to_bytes(self, text: str) -> bytes:
        buf = ctypes.POINTER(ctypes.c_uint8)()
        length = ctypes.c_size_t()
        if self._lib.tts_synth_to_bytes(
            self._ctx, text.encode(), ctypes.byref(buf), ctypes.byref(length)
        ) != 0:
            raise TTSError(self.get_last_error() or "synth_to_bytes failed")
        data = ctypes.string_at(buf, length.value) if buf and length.value > 0 else b""
        if buf:
            self._lib.tts_free_bytes(buf, length.value)
        return data

    # --- playback control ---------------------------------------------

    def stop(self) -> None:
        self._lib.tts_stop(self._ctx)

    def pause(self) -> None:
        self._lib.tts_pause(self._ctx)

    def resume(self) -> None:
        self._lib.tts_resume(self._ctx)

    # --- per-instance settings ----------------------------------------

    def set_voice(self, voice_id: str) -> None:
        self._lib.tts_set_voice(self._ctx, voice_id.encode())

    def set_rate(self, rate: float) -> None:
        self._lib.tts_set_rate(self._ctx, ctypes.c_float(rate))

    def set_pitch(self, pitch: float) -> None:
        self._lib.tts_set_pitch(self._ctx, ctypes.c_float(pitch))

    def set_volume(self, volume: float) -> None:
        self._lib.tts_set_volume(self._ctx, ctypes.c_float(volume))

    # --- callbacks -----------------------------------------------------

    def on_audio(self, callback: Callable[[bytes], None]) -> None:
        """Register a streaming-audio callback. Pass None to clear."""

        if callback is None:
            self._audio_cb_ref = None
            self._lib.tts_set_on_audio(self._ctx, AUDIO_CB(0), None)
            return

        @AUDIO_CB
        def _cb(data, size, _userdata):
            callback(ctypes.string_at(data, size) if data and size else b"")

        self._audio_cb_ref = _cb  # keep alive
        self._lib.tts_set_on_audio(self._ctx, _cb, None)

    def on_boundary(self, callback: Optional[Callable[[str, float, float], None]]) -> None:
        """Register a word-boundary callback. Pass None to clear."""

        if callback is None:
            self._boundary_cb_ref = None
            self._lib.tts_set_on_boundary(self._ctx, BOUNDARY_CB(0), None)
            return

        @BOUNDARY_CB
        def _cb(word, start, end, _userdata):
            callback(word.decode() if word else "", start, end)

        self._boundary_cb_ref = _cb
        self._lib.tts_set_on_boundary(self._ctx, _cb, None)

    def on_start(self, callback: Optional[Callable[[], None]]) -> None:
        """Register a speech-started callback. Pass None to clear."""
        if callback is None:
            self._start_cb_ref = None
            self._lib.tts_set_on_start(self._ctx, VOID_CB(0), None)
            return

        @VOID_CB
        def _cb(_userdata):
            callback()

        self._start_cb_ref = _cb
        self._lib.tts_set_on_start(self._ctx, _cb, None)

    def on_end(self, callback: Optional[Callable[[], None]]) -> None:
        """Register a speech-completed callback. Pass None to clear."""
        if callback is None:
            self._end_cb_ref = None
            self._lib.tts_set_on_end(self._ctx, VOID_CB(0), None)
            return

        @VOID_CB
        def _cb(_userdata):
            callback()

        self._end_cb_ref = _cb
        self._lib.tts_set_on_end(self._ctx, _cb, None)

    def on_error(self, callback: Optional[Callable[[str], None]]) -> None:
        """Register an error callback. Pass None to clear."""
        if callback is None:
            self._error_cb_ref = None
            self._lib.tts_set_on_error(self._ctx, ERROR_CB(0), None)
            return

        @ERROR_CB
        def _cb(error, _userdata):
            callback(error.decode() if error else "unknown error")

        self._error_cb_ref = _cb
        self._lib.tts_set_on_error(self._ctx, _cb, None)

    # --- enumeration ---------------------------------------------------

    def get_voices(self) -> List[Voice]:
        arr = ctypes.POINTER(_TtsVoiceC)()
        count = ctypes.c_int32()
        if self._lib.tts_get_voices(self._ctx, ctypes.byref(arr), ctypes.byref(count)) != 0:
            raise TTSError(self.get_last_error() or "get_voices failed")

        voices: List[Voice] = []
        for i in range(count.value):
            v = arr[i]
            voices.append(
                Voice(
                    id=v.id.decode() if v.id else "",
                    name=v.name.decode() if v.name else "",
                    language=v.language.decode() if v.language else "",
                    gender=v.gender.decode() if v.gender else "",
                    engine=v.engine.decode() if v.engine else "",
                )
            )
        if arr and count.value > 0:
            self._lib.tts_free_voices(arr, count.value)
        return voices

    @classmethod
    def list_engines(cls) -> List[EngineInfo]:
        """Return the list of engines compiled into this build."""
        lib = _load_lib()
        arr = ctypes.POINTER(_TtsEngineInfoC)()
        count = ctypes.c_int32()
        if lib.tts_get_engines(ctypes.byref(arr), ctypes.byref(count)) != 0:
            err = lib.tts_get_last_error(None)
            raise TTSError(err.decode() if err else "tts_get_engines failed")

        engines: List[EngineInfo] = []
        for i in range(count.value):
            e = arr[i]
            keys_json = e.credential_keys_json.decode() if e.credential_keys_json else "[]"
            try:
                keys = json.loads(keys_json)
                if not isinstance(keys, list):
                    keys = []
            except json.JSONDecodeError:
                keys = []
            engines.append(
                EngineInfo(
                    id=e.id.decode() if e.id else "",
                    name=e.name.decode() if e.name else "",
                    needs_credentials=bool(e.needs_credentials),
                    credential_keys=keys,
                )
            )
        if arr and count.value > 0:
            lib.tts_free_engines(arr, count.value)
        return engines

    @classmethod
    def engine_count(cls) -> int:
        """Number of engines available. Convenience over list_engines()."""
        return int(_load_lib().tts_get_engine_count())

    # --- error handling ------------------------------------------------

    def get_last_error(self) -> Optional[str]:
        """Last error for this context, or None if none."""
        ptr = self._lib.tts_get_last_error(self._ctx)
        return ptr.decode() if ptr else None

    @classmethod
    def get_global_last_error(cls) -> Optional[str]:
        """Global last error (used when no context exists, e.g. tts_create)."""
        ptr = _load_lib().tts_get_last_error(None)
        return ptr.decode() if ptr else None


# ---------------------------------------------------------------------------
# Drop-in for the pure-Python tts-wrapper package
# ---------------------------------------------------------------------------

try:
    # Optional dependency: the pure-Python `tts-wrapper` package on PyPI.
    # When present, we expose RustTtsClient that subclasses its TTSClient so
    # projects using tts-wrapper can swap backends with one import change.
    from tts_wrapper.tts import TTSClient as _AbstractTtsClient  # type: ignore
    from tts_wrapper.engines.exceptions import TTSError as _AbstractTTSError  # type: ignore

    _HAS_PURE_PYTHON_TTS = True
except ImportError:  # pragma: no cover
    _HAS_PURE_PYTHON_TTS = False
    _AbstractTtsClient = None  # type: ignore
    _AbstractTTSError = TTSError  # type: ignore


if _HAS_PURE_PYTHON_TTS:

    class RustTtsClient(_AbstractTtsClient):  # type: ignore[misc, valid-type]
        """Drop-in replacement for `tts_wrapper.tts.TTSClient`.

        Subclasses the pure-Python ``tts-wrapper`` interface and routes every
        method to the native Rust backend. Existing code that consumes
        ``tts_wrapper.tts.TTSClient`` can switch backends with::

            # before
            from tts_wrapper import WatsonTTSClient
            client = WatsonTTSClient(credentials=...)

            # after — same surface, Rust backend underneath
            from rust_tts_wrapper import RustTtsClient
            client = RustTtsClient("watson", credentials=...)
        """

        def __init__(self, engine_id: str = "system", credentials: Optional[dict] = None):
            super().__init__()
            self._engine_id = engine_id
            self._native = TTSClient(engine_id, credentials)
            self._credentials = credentials or {}

        # The abstract base requires concrete engines to implement these
        # surface methods; we delegate to the native client.

        def get_voices(self):
            return [
                {
                    "id": v.id,
                    "name": v.name,
                    "language_codes": [{"bcp47": v.language, "iso639_3": v.language.split("-")[0], "display": v.language}],
                    "gender": v.gender,
                    "provider": v.engine,
                }
                for v in self._native.get_voices()
            ]

        def synth_to_bytes(self, text: str, **kwargs) -> bytes:
            voice = kwargs.get("voice")
            if voice:
                self._native.set_voice(voice)
            if "rate" in kwargs:
                self._native.set_rate(float(kwargs["rate"]))
            if "pitch" in kwargs:
                self._native.set_pitch(float(kwargs["pitch"]))
            if "volume" in kwargs:
                self._native.set_volume(float(kwargs["volume"]))
            return self._native.synth_to_bytes(text)

        def speak(self, text: str, **kwargs) -> None:
            voice = kwargs.get("voice")
            if voice:
                self._native.set_voice(voice)
            if "rate" in kwargs:
                self._native.set_rate(float(kwargs["rate"]))
            if "pitch" in kwargs:
                self._native.set_pitch(float(kwargs["pitch"]))
            if "volume" in kwargs:
                self._native.set_volume(float(kwargs["volume"]))
            self._native.speak_sync(text)

        def speak_streamed(self, text: str, callback=None, **kwargs) -> None:
            if callback is not None:
                self._native.on_boundary(callback)
            try:
                self.speak(text, **kwargs)
            finally:
                if callback is not None:
                    self._native.on_boundary(None)

        def stop(self) -> None:
            self._native.stop()

        def pause(self) -> None:
            self._native.pause()

        def resume(self) -> None:
            self._native.resume()

        def close(self) -> None:
            self._native.close()


# ---------------------------------------------------------------------------
# Backwards-compatible module-level helper
# ---------------------------------------------------------------------------


def list_engines() -> int:
    """Number of engines available.

    Deprecated: prefer :meth:`TTSClient.list_engines` which returns the full
    list of engine descriptors. Kept for backwards compatibility with
    earlier versions of this module.
    """
    return TTSClient.engine_count()
