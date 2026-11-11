// Node.js bindings for the rust-tts-wrapper C ABI.
//
// Architecture: dlopen the shared library at runtime (koffi) and wrap the
// flat C surface in an EventEmitter-based client. No native compilation
// is required for this package — the library is built once with cargo
// (see bindings/README.md) and located at load time.
//
// Library resolution order:
//   1. TTS_WRAPPER_LIB env var (absolute or relative path)
//   2. <pkg>/runtimes/<platform>/<name>  ( packaged layout )
//   3. <repo>/target/release/<name>      ( dev layout )
//   4. <repo>/target/debug/<name>
//   5. plain name (falls back to the OS loader's search path)

"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { EventEmitter } = require("node:events");
const koffi = require("koffi");

const PLATFORM_NAMES = {
  linux: "librust_tts_wrapper.so",
  darwin: "librust_tts_wrapper.dylib",
  win32: "rust_tts_wrapper.dll",
};

/**
 * Stable error codes (GH #69) — append-only u32 values from the C ABI.
 * Frozen object (not a const enum) so `isolatedModules` consumers and
 * plain JS both work.
 */
const TtsErrorCode = Object.freeze({
  Unknown: 0,
  MissingConfiguration: 1,
  InvalidConfiguration: 2,
  CredentialsRejected: 3,
  NetworkError: 4,
  ModelNotFound: 5,
  MissingNativeDependency: 6,
  EngineNotAvailable: 7,
  LanguageNotSupported: 8,
  EngineFailure: 9,
});

/** Known setup-field kinds (GH #68); anything else degrades to "text". */
const KINDS = new Set([
  "secret",
  "filePath",
  "directoryPath",
  "url",
  "choice",
  "number",
  "text",
]);

/** Resolve the shared library path for this platform. */
function resolveLibraryPath(explicit) {
  const candidates = [];
  if (explicit) {
    // An explicit path is a contract: fail loudly, never fall through.
    if (!fs.existsSync(explicit)) {
      throw new Error(`rust_tts_wrapper library not found at ${explicit}`);
    }
    return explicit;
  }
  const base = PLATFORM_NAMES[process.platform];
  if (!base) {
    throw new Error(`unsupported platform: ${process.platform}`);
  }
  const pkgRoot = path.join(__dirname, "..");
  const repoRoot = path.join(pkgRoot, "..", "..");
  candidates.push(path.join(pkgRoot, "runtimes", `${process.platform}-${process.arch}`, base));
  candidates.push(path.join(repoRoot, "target", "release", base));
  candidates.push(path.join(repoRoot, "target", "debug", base));
  candidates.push(base);
  for (const c of candidates) {
    if (!c.includes(path.sep)) return c; // bare name → OS search path
    if (fs.existsSync(c)) return c;
  }
  throw new Error(
    `rust_tts_wrapper library not found; set TTS_WRAPPER_LIB or build with cargo (tried: ${candidates.join(", ")})`,
  );
}

// koffi's named types AND callback prototypes are process-global:
// declare them exactly once, lazily.
let koffiTypes = null;
function getKoffiTypes() {
  if (koffiTypes) return koffiTypes;
  koffiTypes = {
    TtsVoice: koffi.struct("tts_voice", {
      id: koffi.pointer("const char"),
      name: koffi.pointer("const char"),
      language: koffi.pointer("const char"),
      gender: koffi.pointer("const char"),
      engine: koffi.pointer("const char"),
    }),
    TtsEngineInfo: koffi.struct("tts_engine_info", {
      id: koffi.pointer("const char"),
      name: koffi.pointer("const char"),
      needs_credentials: koffi.types.uint8,
      credential_keys_json: koffi.pointer("const char"),
    }),
    cbProtos: {
      audio: koffi.proto("void audio_cb(const uint8_t *data, uintptr_t len, void *userdata)"),
      boundary: koffi.proto(
        "void boundary_cb(const char *word, int32_t char_offset, int32_t char_len, float start_s, float end_s, int32_t estimated, void *userdata)",
      ),
      mark: koffi.proto(
        "void mark_cb(const char *name, int32_t char_offset, float start_s, float end_s, void *userdata)",
      ),
      viseme: koffi.proto("void viseme_cb(int32_t viseme_id, float offset_s, void *userdata)"),
      start: koffi.proto("void start_cb(void *userdata)"),
      end: koffi.proto("void end_cb(void *userdata)"),
      error: koffi.proto("void error_cb(const char *message, void *userdata)"),
    },
  };
  return koffiTypes;
}

/** Load the C ABI. Exposed for tests and advanced embedding. */
function loadLibrary(explicitPath) {
  const libPath = resolveLibraryPath(explicitPath ?? process.env.TTS_WRAPPER_LIB);
  const lib = koffi.load(libPath);
  const { TtsVoice, TtsEngineInfo, cbProtos } = getKoffiTypes();

  const protos = {
    tts_create: lib.func("void *tts_create(const char *engine_id, const char *credentials_json)"),
    tts_destroy: lib.func("void tts_destroy(void *ctx)"),
    tts_speak: lib.func("int32_t tts_speak(void *ctx, const char *text)"),
    tts_speak_ssml: lib.func("int32_t tts_speak_ssml(void *ctx, const char *ssml)"),
    tts_speak_sync: lib.func("int32_t tts_speak_sync(void *ctx, const char *text)"),
    tts_stop: lib.func("void tts_stop(void *ctx)"),
    tts_pause: lib.func("void tts_pause(void *ctx)"),
    tts_resume: lib.func("void tts_resume(void *ctx)"),
    tts_synth_to_bytes: lib.func(
      "int32_t tts_synth_to_bytes(void *ctx, const char *text, _Out_ uint8_t **out_bytes, _Out_ uintptr_t *out_len)",
    ),
    tts_free_bytes: lib.func("void tts_free_bytes(uint8_t *bytes, uintptr_t len)"),
    tts_set_voice: lib.func("void tts_set_voice(void *ctx, const char *voice_id)"),
    tts_set_rate: lib.func("void tts_set_rate(void *ctx, float rate)"),
    tts_set_pitch: lib.func("void tts_set_pitch(void *ctx, float pitch)"),
    tts_set_volume: lib.func("void tts_set_volume(void *ctx, float volume)"),
    tts_get_voices: lib.func(
      "int32_t tts_get_voices(void *ctx, _Out_ tts_voice **out_voices, _Out_ int32_t *out_count)",
    ),
    tts_free_voices: lib.func("void tts_free_voices(tts_voice *voices, int32_t count)"),
    tts_get_engine_count: lib.func("int32_t tts_get_engine_count()"),
    tts_get_engines: lib.func(
      "int32_t tts_get_engines(_Out_ tts_engine_info **out_engines, _Out_ int32_t *out_count)",
    ),
    tts_free_engines: lib.func("void tts_free_engines(tts_engine_info *engines, int32_t count)"),
    tts_get_last_error: lib.func("const char *tts_get_last_error(void *ctx)"),
  };

  const setters = {
    audio: lib.func("void tts_set_on_audio(void *ctx, audio_cb *cb, void *userdata)"),
    boundary: lib.func("void tts_set_on_boundary(void *ctx, boundary_cb *cb, void *userdata)"),
    mark: lib.func("void tts_set_on_mark(void *ctx, mark_cb *cb, void *userdata)"),
    viseme: lib.func("void tts_set_on_viseme(void *ctx, viseme_cb *cb, void *userdata)"),
    start: lib.func("void tts_set_on_start(void *ctx, start_cb *cb, void *userdata)"),
    end: lib.func("void tts_set_on_end(void *ctx, end_cb *cb, void *userdata)"),
    error: lib.func("void tts_set_on_error(void *ctx, error_cb *cb, void *userdata)"),
  };

  // The GH #68/#69 symbols are additive: an older native build (pre
  // PR #72) does not export them. Probe instead of letting the first
  // koffi lookup fail cryptically, and give the methods a clear error.
  const PARITY_PRECEDES = "native rust_tts_wrapper predates the GH #68/#69 ABI (PR #72); rebuild the library";
  const optional = (decl) => {
    try {
      return lib.func(decl);
    } catch {
      return null;
    }
  };
  const parity = {
    tts_get_last_error_code: optional("uint32_t tts_get_last_error_code(void *ctx)"),
    tts_get_last_error_detail_json: optional(
      "void *tts_get_last_error_detail_json(void *ctx)",
    ),
    tts_free_string: optional("void tts_free_string(void *s)"),
    tts_get_engine_setups_json: optional("void *tts_get_engine_setups_json()"),
    tts_check_credentials: optional("int32_t tts_check_credentials(void *ctx)"),
  };
  const needParity = (name) => {
    if (!parity[name]) throw new Error(PARITY_PRECEDES);
    return parity[name];
  };

  return {
    lib,
    libPath,
    protos,
    cbProtos,
    setters,
    parity: { fns: parity, needParity },
    TtsVoice,
    TtsEngineInfo,
  };
}

// koffi auto-converts `const char *` return values and struct fields to
// JS strings (null when the pointer is null).
function cstr(s) {
  return s ?? "";
}

/**
 * Structured error with a stable code (GH #69). `code` mirrors the
 * C ABI's append-only u32; `fieldKey` names the offending settings row
 * for MissingConfiguration; `displayMessage` is safe to show (the raw
 * message is secret-redacted by the native layer).
 */
class TtsError extends Error {
  /**
   * @param {string} message fallback message (used when no structured
   *   detail is available)
   * @param {import("./index.d.ts").TtsErrorDetail|null} detail
   */
  constructor(message, detail) {
    super(detail && detail.message ? detail.message : message);
    this.name = "TtsError";
    this.code = detail ? detail.code : 0;
    this.codeName = detail ? detail.codeName : "Unknown";
    this.fieldKey = detail ? detail.fieldKey : "";
    this.engineId = detail ? detail.engineId : "";
    this.displayMessage =
      detail && detail.displayMessage && detail.displayMessage.length > 0
        ? detail.displayMessage
        : null;
  }
}

/**
 * Read an OWNED NUL-terminated C string from the ABI and free the
 * native allocation. `ptr` must come from a void*-declared proto (see
 * loadLibrary) so koffi hands us the raw pointer instead of a detached
 * JS string. koffi 2.16's decode(ptr, 'const char *') segfaults on
 * runtime pointers, so the string is assembled from byte chunks.
 *
 * @returns {string} the decoded string ("" when empty)
 */
function readOwnedCString(abi, ptr, maxLen = 1 << 22) {
  if (!ptr) return "";
  const freeString = abi.parity.needParity("tts_free_string");
  const CHUNK = 8192;
  const parts = [];
  try {
    let offset = 0;
    while (offset < maxLen) {
      const step = Math.min(CHUNK, maxLen - offset);
      const bytes = koffi.decode(ptr, offset, "uint8_t", step);
      const nul = bytes.indexOf(0);
      if (nul >= 0) {
        parts.push(Buffer.from(bytes.subarray(0, nul)));
        break;
      }
      parts.push(Buffer.from(bytes));
      offset += step;
    }
    return Buffer.concat(parts).toString("utf8");
  } finally {
    freeString(ptr);
  }
}

/**
 * Read an OWNED JSON string from the ABI and parse it (null on empty
 * or malformed — forward compatible).
 */
function readOwnedJson(abi, ptr) {
  const s = readOwnedCString(abi, ptr);
  if (!s.length) return null;
  try {
    return JSON.parse(s);
  } catch {
    return null;
  }
}

/**
 * Read and parse an owned DETAIL JSON string, mapping the native shape
 * (`{"code","name","message",…}`) onto the public one (`codeName`) —
 * the one place the key differs (GH #73 review).
 */
function readOwnedDetail(abi, ptr) {
  const parsed = readOwnedJson(abi, ptr);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
  return {
    code: typeof parsed.code === "number" ? parsed.code : 0,
    codeName: typeof parsed.name === "string" ? parsed.name : "Unknown",
    message: typeof parsed.message === "string" ? parsed.message : "",
    displayMessage:
      typeof parsed.displayMessage === "string" ? parsed.displayMessage : "",
    engineId: typeof parsed.engineId === "string" ? parsed.engineId : "",
    fieldKey: typeof parsed.fieldKey === "string" ? parsed.fieldKey : "",
  };
}

/**
 * Event-emitting client over one engine instance (a `tts_ctx`).
 *
 * Events: "audio" (Buffer), "boundary" ({word, charOffset, charLen,
 * startSec, endSec, estimated}), "mark" ({name, charOffset, startSec,
 * endSec}), "viseme" ({id, offsetSec}), "start", "end", "error" (string).
 */
class TtsClient extends EventEmitter {
  /**
   * @param {object} [options]
   * @param {string} [options.engineId="system"]
   * @param {Record<string, string>} [options.credentials]
   * @param {object} [options.library] preloaded ABI (from loadLibrary)
   */
  constructor({ engineId = "system", credentials = {}, library } = {}) {
    super();
    this._abi = library ?? loadLibrary();
    this._ctx = this._abi.protos.tts_create(
      engineId,
      JSON.stringify(credentials ?? {}),
    );
    if (!this._ctx) {
      // Global (create-time) error: attach the structured detail when
      // the native build provides it (GH #69).
      const detail = TtsClient.globalLastErrorDetail(this._abi);
      const fallback = `tts_create(${engineId}) failed: ${
        TtsClient.globalLastError(this._abi) ?? "unknown"
      }`;
      throw new TtsError(fallback, detail);
    }
    this._registered = []; // koffi callback handles kept alive
    this._closed = false;
    this._registerAllEvents();
  }

  // --- synthesis ---------------------------------------------------------

  speak(text) {
    this._throwIfClosed();
    const rc = this._abi.protos.tts_speak(this._ctx, text);
    if (rc !== 0) throw this._structuredError("tts_speak failed");
  }

  speakSsml(ssml) {
    this._throwIfClosed();
    const rc = this._abi.protos.tts_speak_ssml(this._ctx, ssml);
    if (rc !== 0) throw this._structuredError("tts_speak_ssml failed");
  }

  speakSync(text) {
    this._throwIfClosed();
    const rc = this._abi.protos.tts_speak_sync(this._ctx, text);
    if (rc !== 0) throw this._structuredError("tts_speak_sync failed");
  }

  /** Synthesise to a Buffer. Returns an empty Buffer on zero audio. */
  synthToBytes(text) {
    this._throwIfClosed();
    const out = [null];
    const outLen = [0n];
    const rc = this._abi.protos.tts_synth_to_bytes(this._ctx, text, out, outLen);
    if (rc !== 0) throw this._structuredError("tts_synth_to_bytes failed");
    const len = Number(outLen[0]);
    const ptr = out[0];
    if (!ptr || len === 0) return Buffer.alloc(0);
    try {
      return Buffer.from(koffi.decode(ptr, "uint8_t", Number(len)));
    } finally {
      this._abi.protos.tts_free_bytes(ptr, BigInt(len));
    }
  }

  // --- playback control --------------------------------------------------

  stop() {
    this._throwIfClosed();
    this._abi.protos.tts_stop(this._ctx);
  }
  pause() {
    this._throwIfClosed();
    this._abi.protos.tts_pause(this._ctx);
  }
  resume() {
    this._throwIfClosed();
    this._abi.protos.tts_resume(this._ctx);
  }

  // --- settings ----------------------------------------------------------

  setVoice(voiceId) {
    this._throwIfClosed();
    this._abi.protos.tts_set_voice(this._ctx, voiceId ?? "");
  }
  setRate(rate) {
    this._throwIfClosed();
    this._abi.protos.tts_set_rate(this._ctx, rate);
  }
  setPitch(pitch) {
    this._throwIfClosed();
    this._abi.protos.tts_set_pitch(this._ctx, pitch);
  }
  setVolume(volume) {
    this._throwIfClosed();
    this._abi.protos.tts_set_volume(this._ctx, volume);
  }

  // --- enumeration -------------------------------------------------------

  /** @returns {{id:string,name:string,language:string,gender:string,engine:string}[]} */
  getVoices() {
    this._throwIfClosed();
    const out = [null];
    const outCount = [0];
    const rc = this._abi.protos.tts_get_voices(this._ctx, out, outCount);
    if (rc !== 0) throw this._structuredError("tts_get_voices failed");
    const count = outCount[0];
    const arr = out[0];
    if (!arr || count <= 0) return [];
    try {
      const voices = koffi.decode(arr, this._abi.TtsVoice, count);
      return voices.map((v) => ({
        id: cstr(v.id),
        name: cstr(v.name),
        language: cstr(v.language),
        gender: cstr(v.gender),
        engine: cstr(v.engine),
      }));
    } finally {
      this._abi.protos.tts_free_voices(arr, count);
    }
  }

  /** @returns {{id:string,name:string,needsCredentials:boolean,credentialKeys:string[]}[]} */
  static listEngines(abi) {
    const a = abi ?? loadLibrary();
    const out = [null];
    const outCount = [0];
    const rc = a.protos.tts_get_engines(out, outCount);
    if (rc !== 0) {
      throw new Error(TtsClient.globalLastError(a) ?? "tts_get_engines failed");
    }
    const count = outCount[0];
    const arr = out[0];
    if (!arr || count <= 0) return [];
    try {
      const engines = koffi.decode(arr, a.TtsEngineInfo, count);
      return engines.map((e) => ({
        id: cstr(e.id),
        name: cstr(e.name),
        needsCredentials: e.needs_credentials !== 0,
        credentialKeys: JSON.parse(cstr(e.credential_keys_json) || "[]"),
      }));
    } finally {
      a.protos.tts_free_engines(arr, count);
    }
  }

  static engineCount(abi) {
    return (abi ?? loadLibrary()).protos.tts_get_engine_count();
  }

  /**
   * Typed setup schema for every engine compiled into this build
   * (GH #68): fields with kinds (secret / filePath / directoryPath /
   * url / choice / number / text) + capability flags, so a settings UI
   * renders engine rows without a provider switch.
   */
  static engineSetups(abi) {
    const a = abi ?? loadLibrary();
    const ptr = a.parity.needParity("tts_get_engine_setups_json")();
    const parsed = readOwnedJson(a, ptr);
    if (!Array.isArray(parsed)) {
      throw new TtsError("tts_get_engine_setups_json returned unparseable JSON");
    }
    // Tolerant normalisation: unknown kinds degrade to "text", missing
    // properties get defaults (forward compatible with newer natives).
    return parsed.map((s) => ({
      id: cstr(s.id),
      name: cstr(s.name),
      needsCredentials: s.needsCredentials === true,
      fields: (Array.isArray(s.fields) ? s.fields : []).map((f) => ({
        key: cstr(f.key),
        label: cstr(f.label),
        kind: KINDS.has(f.kind) ? f.kind : "text",
        required: f.required === true,
        defaultValue: typeof f.defaultValue === "string" ? f.defaultValue : undefined,
        choices: Array.isArray(f.choices) ? f.choices.map(cstr) : [],
        placeholder: typeof f.placeholder === "string" ? f.placeholder : undefined,
        helpText: typeof f.helpText === "string" ? f.helpText : undefined,
      })),
      capabilities: {
        supportsRate: s.capabilities?.supportsRate === true,
        supportsPitch: s.capabilities?.supportsPitch === true,
        supportsVolume: s.capabilities?.supportsVolume === true,
        supportsWordTiming: s.capabilities?.supportsWordTiming === true,
        supportsSsml: s.capabilities?.supportsSsml === true,
        supportsStreaming: s.capabilities?.supportsStreaming === true,
        requiresInternet: s.capabilities?.requiresInternet === true,
        staticVoiceCatalog: s.capabilities?.staticVoiceCatalog === true,
        notes: typeof s.capabilities?.notes === "string" ? s.capabilities.notes : undefined,
      },
      requiredAny: Array.isArray(s.requiredAny)
        ? s.requiredAny.map((g) => (Array.isArray(g) ? g.map(cstr) : []))
        : [],
    }));
  }

  /** The setup schema for one engine id, or null when unknown (GH #68). */
  static engineSetup(engineId, abi) {
    return TtsClient.engineSetups(abi).find((s) => s.id === engineId) ?? null;
  }

  // --- errors / lifecycle ------------------------------------------------

  lastError() {
    return this._lastError();
  }

  /** Stable classification of the last error (0 = none, GH #69). */
  lastErrorCode() {
    this._throwIfClosed();
    return this._abi.parity.needParity("tts_get_last_error_code")(this._ctx);
  }

  /** Structured detail of the last error, or null (GH #69). */
  lastErrorDetail() {
    this._throwIfClosed();
    const ptr = this._abi.parity.needParity("tts_get_last_error_detail_json")(this._ctx);
    return readOwnedDetail(this._abi, ptr);
  }

  /**
   * Tri-state credential check (GH #69): "verified" | "rejected" |
   * "unverified" | { status: "error", error }. Static catalogs answer
   * "unverified" — enumerating them proves nothing about the key.
   */
  checkCredentials() {
    this._throwIfClosed();
    const rc = this._abi.parity.needParity("tts_check_credentials")(this._ctx);
    switch (rc) {
      case 1:
        return { status: "verified" };
      case 0:
        return { status: "rejected" };
      case 2:
        return { status: "unverified" };
      default:
        return { status: "error", error: this.lastErrorDetail() };
    }
  }

  static globalLastError(abi) {
    const p = (abi ?? loadLibrary()).protos.tts_get_last_error(null);
    const s = cstr(p);
    return s.length ? s : null;
  }

  /** Stable code of the global (create-time) error, 0 = none (GH #69). */
  static globalLastErrorCode(abi) {
    return (abi ?? loadLibrary()).parity.needParity("tts_get_last_error_code")(null);
  }

  /** Structured detail of the global error, or null (GH #69). */
  static globalLastErrorDetail(abi) {
    const a = abi ?? loadLibrary();
    const ptr = a.parity.needParity("tts_get_last_error_detail_json")(null);
    return readOwnedDetail(a, ptr);
  }

  close() {
    if (this._closed) return;
    this._closed = true;
    // Clear every callback so no dangling trampoline fires after destroy.
    for (const key of Object.keys(this._abi.setters)) {
      try {
        this._abi.setters[key](this._ctx, null, null);
      } catch {
        /* best effort */
      }
    }
    for (const handle of this._registered) {
      try {
        koffi.unregister(handle);
      } catch {
        /* best effort */
      }
    }
    this._registered = [];
    this._abi.protos.tts_destroy(this._ctx);
    this._ctx = null;
  }

  [Symbol.dispose]() {
    this.close();
  }

  // --- internal ----------------------------------------------------------

  _lastError() {
    const p = this._abi.protos.tts_get_last_error(this._ctx);
    const s = cstr(p);
    return s.length ? s : null;
  }

  /** Build a TtsError carrying the structured detail (GH #69). */
  _structuredError(fallback) {
    return new TtsError(`${this._lastError() ?? fallback}`, this.lastErrorDetail());
  }

  _throwIfClosed() {
    if (this._closed) throw new Error("TtsClient is closed");
  }

  _registerAllEvents() {
    const { cbProtos, setters } = this._abi;

    const reg = (key, fn) => {
      const handle = koffi.register(fn, koffi.pointer(cbProtos[key]));
      this._registered.push(handle);
      setters[key](this._ctx, handle, null);
    };

    reg("audio", (data, len) => {
      const n = Number(len);
      this.emit("audio", n > 0 && data ? Buffer.from(data.subarray(0, n)) : Buffer.alloc(0));
    });
    reg("boundary", (word, charOffset, charLen, startS, endS, estimated) => {
      this.emit("boundary", {
        word: cstr(word),
        charOffset,
        charLen,
        startSec: startS,
        endSec: endS,
        estimated: estimated !== 0,
      });
    });
    reg("mark", (name, charOffset, startS, endS) => {
      this.emit("mark", {
        name: cstr(name),
        charOffset,
        startSec: startS,
        endSec: endS,
      });
    });
    reg("viseme", (id, offsetS) => {
      this.emit("viseme", { id, offsetSec: offsetS });
    });
    reg("start", () => this.emit("start"));
    reg("end", () => this.emit("end"));
    reg("error", (msg) => this.emit("error", cstr(msg)));
  }
}

module.exports = { TtsClient, TtsError, TtsErrorCode, loadLibrary, resolveLibraryPath };
