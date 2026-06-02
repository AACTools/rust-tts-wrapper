"""Python bindings for rust-tts-wrapper via ctypes."""

import ctypes
import json
import platform
from pathlib import Path
from typing import Callable, Dict, List, Optional

_lib = None

AUDIO_CB = ctypes.CFUNCTYPE(None, ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t, ctypes.c_void_p)
BOUNDARY_CB = ctypes.CFUNCTYPE(None, ctypes.c_char_p, ctypes.c_float, ctypes.c_float, ctypes.c_void_p)


def _load_lib():
    global _lib
    if _lib is not None:
        return _lib
    if platform.system() == "Linux":
        lib_path = Path(__file__).parent / "librust_tts_wrapper.so"
    elif platform.system() == "Darwin":
        lib_path = Path(__file__).parent / "librust_tts_wrapper.dylib"
    else:
        lib_path = Path(__file__).parent / "rust_tts_wrapper.dll"
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
    _lib.tts_get_voices.restype = ctypes.c_int32
    _lib.tts_get_voices.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p)), ctypes.POINTER(ctypes.c_int32)]
    _lib.tts_free_voices.restype = None
    _lib.tts_free_voices.argtypes = [ctypes.POINTER(ctypes.c_void_p), ctypes.c_int32]
    _lib.tts_get_engine_count.restype = ctypes.c_int32
    _lib.tts_get_last_error.restype = ctypes.c_char_p
    return _lib


class Voice:
    """A TTS voice with metadata."""
    __slots__ = ("id", "name", "language", "gender", "engine")

    def __init__(self, id: str, name: str, language: str, gender: str, engine: str = ""):
        self.id = id
        self.name = name
        self.language = language
        self.gender = gender
        self.engine = engine

    def __repr__(self):
        return f"Voice(id={self.id!r}, name={self.name!r}, engine={self.engine!r})"


class TTSClient:
    """Python TTS client wrapping the Rust C library."""

    def __init__(self, engine_id: str = "system", credentials: Optional[dict] = None):
        self._lib = _load_lib()
        creds_json = json.dumps(credentials or {})
        self._ctx = self._lib.tts_create(
            engine_id.encode(), creds_json.encode()
        )
        if not self._ctx:
            err = self._lib.tts_get_last_error()
            msg = err.decode() if err else "Unknown error"
            raise RuntimeError(f"Failed to create TTS engine: {msg}")
        self._audio_cb = None
        self._boundary_cb = None

    def __del__(self):
        if hasattr(self, "_ctx") and self._ctx:
            self._lib.tts_destroy(self._ctx)

    def speak(self, text: str) -> None:
        result = self._lib.tts_speak(self._ctx, text.encode())
        if result != 0:
            raise RuntimeError("Speech synthesis failed")

    def speak_sync(self, text: str) -> None:
        result = self._lib.tts_speak_sync(self._ctx, text.encode())
        if result != 0:
            raise RuntimeError("Speech synthesis failed")

    def stop(self) -> None:
        self._lib.tts_stop(self._ctx)

    def set_voice(self, voice_id: str) -> None:
        self._lib.tts_set_voice(self._ctx, voice_id.encode())

    def set_rate(self, rate: float) -> None:
        self._lib.tts_set_rate(self._ctx, ctypes.c_float(rate))

    def set_pitch(self, pitch: float) -> None:
        self._lib.tts_set_pitch(self._ctx, ctypes.c_float(pitch))

    def set_volume(self, volume: float) -> None:
        self._lib.tts_set_volume(self._ctx, ctypes.c_float(volume))

    def on_audio(self, callback: Callable[[bytes], None]) -> None:
        @AUDIO_CB
        def _cb(data, size, _userdata):
            callback(ctypes.string_at(data, size))
        self._audio_cb = _cb
        self._lib.tts_set_on_audio(self._ctx, _cb, None)

    def on_boundary(self, callback: Callable[[str, float, float], None]) -> None:
        @BOUNDARY_CB
        def _cb(word, start, end, _userdata):
            callback(word.decode() if word else "", start, end)
        self._boundary_cb = _cb
        self._lib.tts_set_on_boundary(self._ctx, _cb, None)


def list_engines() -> int:
    lib = _load_lib()
    return lib.tts_get_engine_count()
