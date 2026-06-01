"""Python bindings for rust-tts-wrapper via ctypes."""

import ctypes
import json
import platform
from pathlib import Path
from typing import Optional

_lib = None

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
    _lib.tts_speak.restype = ctypes.c_int
    _lib.tts_speak.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    _lib.tts_stop.restype = None
    _lib.tts_stop.argtypes = [ctypes.c_void_p]
    _lib.tts_set_voice.restype = None
    _lib.tts_set_voice.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    _lib.tts_set_rate.restype = None
    _lib.tts_set_rate.argtypes = [ctypes.c_void_p, ctypes.c_float]
    _lib.tts_get_engine_count.restype = ctypes.c_int
    _lib.tts_get_last_error.restype = ctypes.c_char_p
    return _lib


class Voice:
    __slots__ = ("id", "name", "language", "gender")
    def __init__(self, id: str, name: str, language: str, gender: str):
        self.id = id
        self.name = name
        self.language = language
        self.gender = gender
    def __repr__(self):
        return f"Voice(id={self.id!r}, name={self.name!r})"


class TTSClient:
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

    def __del__(self):
        if hasattr(self, "_ctx") and self._ctx:
            self._lib.tts_destroy(self._ctx)

    def speak(self, text: str) -> None:
        result = self._lib.tts_speak(self._ctx, text.encode())
        if result != 0:
            raise RuntimeError("Speech synthesis failed")

    def stop(self) -> None:
        self._lib.tts_stop(self._ctx)

    def set_voice(self, voice_id: str) -> None:
        self._lib.tts_set_voice(self._ctx, voice_id.encode())

    def set_rate(self, rate: float) -> None:
        self._lib.tts_set_rate(self._ctx, ctypes.c_float(rate))


def list_engines():
    lib = _load_lib()
    count = lib.tts_get_engine_count()
    return count
