#ifndef TTS_WRAPPER_H
#define TTS_WRAPPER_H

/* Auto-generated. Do not edit. */

#include <stdint.h>
#include <stdbool.h>

typedef struct tts_ctx tts_ctx;

/**
 * C-compatible voice descriptor returned by [`tts_get_voices`](crate::tts_get_voices).
 */
typedef struct tts_voice {
  /**
   * Voice identifier (owned C string).
   */
  char *id;
  /**
   * Voice name (owned C string).
   */
  char *name;
  /**
   * Language tag (owned C string).
   */
  char *language;
  /**
   * Gender (owned C string).
   */
  char *gender;
  /**
   * Engine identifier (owned C string).
   */
  char *engine;
} tts_voice;

/**
 * Opaque context holding an engine instance and its per-instance settings.
 */
typedef void (*CAudioCb)(const uint8_t*, uintptr_t, void*);

typedef void (*CBoundaryCb)(const char*, float, float, void*);

/**
 * C-compatible engine descriptor returned by [`tts_get_engines`](crate::tts_get_engines).
 */
typedef struct tts_engine_info {
  /**
   * Engine identifier (owned C string).
   */
  char *id;
  /**
   * Engine name (owned C string).
   */
  char *name;
  /**
   * Whether credentials are required.
   */
  bool needs_credentials;
  /**
   * JSON array of credential key names (owned C string).
   */
  char *credential_keys_json;
} tts_engine_info;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

/**
 * Create a new TTS engine instance.
 *
 * Returns an opaque context pointer on success, or null on failure.
 * Call [`tts_get_last_error`] to retrieve the error message on failure.
 *
 * # Safety
 *
 * `engine_id` must be a valid null-terminated C string.
 * `credentials_json` may be null or a valid null-terminated JSON string.
 */
struct tts_ctx *tts_create(const char *engine_id, const char *credentials_json);

/**
 * Destroy a TTS context and free all associated resources.
 *
 * # Safety
 *
 * `ctx` must be a pointer previously returned by [`tts_create`],
 * or null (no-op).
 */
void tts_destroy(struct tts_ctx *ctx);

/**
 * Speak `text` asynchronously using the engine in `ctx`.
 *
 * Returns 0 on success, -1 on failure.
 *
 * # Safety
 *
 * `ctx` must be a valid pointer from [`tts_create`].
 * `text` must be a valid null-terminated C string.
 */
int32_t tts_speak(struct tts_ctx *ctx, const char *text);

/**
 * Speak `text` synchronously (blocks until complete).
 *
 * Returns 0 on success, -1 on failure.
 *
 * # Safety
 *
 * `ctx` must be a valid pointer from [`tts_create`].
 * `text` must be a valid null-terminated C string.
 */
int32_t tts_speak_sync(struct tts_ctx *ctx, const char *text);

/**
 * Stop any in-progress speech.
 *
 * # Safety
 *
 * `ctx` must be a valid pointer from [`tts_create`].
 */
void tts_stop(struct tts_ctx *ctx);

/**
 * Retrieve the list of available voices for the engine.
 *
 * On success, writes a heap-allocated array to `*out_voices` and its length
 * to `*out_count`. Caller must free with [`tts_free_voices`].
 *
 * Returns 0 on success, -1 on failure.
 *
 * # Safety
 *
 * `ctx` must be valid. `out_voices` and `out_count` must be non-null.
 */
int32_t tts_get_voices(struct tts_ctx *ctx, struct tts_voice **out_voices, int32_t *out_count);

/**
 * Free a voice array previously returned by [`tts_get_voices`].
 *
 * # Safety
 *
 * `voices` must be a pointer from `tts_get_voices` with the matching `count`.
 */
void tts_free_voices(struct tts_voice *voices, int32_t count);

/**
 * Set the voice for subsequent speak calls.
 *
 * # Safety
 *
 * `ctx` must be valid. `voice_id` must be a valid null-terminated C string.
 */
void tts_set_voice(struct tts_ctx *ctx, const char *voice_id);

/**
 * Set the speech rate (1.0 = normal).
 *
 * # Safety
 *
 * `ctx` must be valid.
 */
void tts_set_rate(struct tts_ctx *ctx, float rate);

/**
 * Set the speech pitch (1.0 = normal).
 *
 * # Safety
 *
 * `ctx` must be valid.
 */
void tts_set_pitch(struct tts_ctx *ctx, float pitch);

/**
 * Set the speech volume (1.0 = normal).
 *
 * # Safety
 *
 * `ctx` must be valid.
 */
void tts_set_volume(struct tts_ctx *ctx, float volume);

/**
 * Set the callback for streaming audio chunks.
 *
 * # Safety
 * `ctx` must be valid.
 */
void tts_set_on_audio(struct tts_ctx *ctx, CAudioCb cb, void *userdata);

/**
 * Set the callback for word boundary events.
 *
 * # Safety
 * `ctx` must be valid.
 */
void tts_set_on_boundary(struct tts_ctx *ctx, CBoundaryCb cb, void *userdata);

/**
 * Return the number of registered engines.
 */
int32_t tts_get_engine_count(void);

/**
 * Get the list of available engine descriptors.
 *
 * On success, writes a heap-allocated array to `*out_engines` and its length
 * to `*out_count`. Caller must free with [`tts_free_engines`].
 *
 * Returns 0 on success, -1 on failure.
 *
 * # Safety
 *
 * `out_engines` and `out_count` must be non-null.
 */
int32_t tts_get_engines(struct tts_engine_info **out_engines, int32_t *out_count);

/**
 * Free an engine info array previously returned by [`tts_get_engines`].
 *
 * # Safety
 *
 * `engines` must be a pointer from `tts_get_engines` with the matching `count`.
 */
void tts_free_engine_info(struct tts_engine_info *engines, int32_t count);

/**
 * Return the last error message as a C string, or null if none.
 *
 * If ctx is provided, returns per-context error. If ctx is null,
 * returns global error (for tts_create failures).
 *
 * The returned pointer is valid until the next call to any TTS function.
 *
 * @param ctx Context pointer, or null for global error.
 */
const char *tts_get_last_error(tts_ctx *ctx);

/**
 * Pause in-progress speech.
 *
 * # Safety
 * `ctx` must be valid.
 */
void tts_pause(struct tts_ctx *ctx);

/**
 * Resume paused speech.
 *
 * # Safety
 * `ctx` must be valid.
 */
void tts_resume(struct tts_ctx *ctx);

/**
 * Synthesize text to audio bytes without playback.
 * Writes a heap-allocated buffer to `*out_bytes` and its length to `*out_len`.
 * Caller must free with [`tts_free_bytes`].
 * Returns 0 on success, -1 on failure.
 *
 * # Safety
 * `ctx` must be valid. `out_bytes` and `out_len` must be non-null.
 */
int32_t tts_synth_to_bytes(struct tts_ctx *ctx,
                           const char *text,
                           uint8_t **out_bytes,
                           uintptr_t *out_len);

/**
 * Free a byte buffer returned by [`tts_synth_to_bytes`].
 *
 * # Safety
 * `bytes` must be from `tts_synth_to_bytes` with the matching `len`.
 */
void tts_free_bytes(uint8_t *bytes, uintptr_t len);

extern void *avsynth_create(void);

extern void avsynth_destroy(void *handle);

extern void avsynth_speak(void *handle,
                          const uint8_t *text,
                          const uint8_t *voice_id,
                          float rate,
                          float pitch,
                          float volume);

extern void avsynth_stop(void *handle);

extern void avsynth_pause(void *handle);

extern void avsynth_resume(void *handle);

extern int32_t avsynth_voice_count(void *handle);

extern int32_t avsynth_get_voice(void *handle,
                                 int32_t index,
                                 uint8_t *id_buf,
                                 int32_t id_buf_len,
                                 uint8_t *name_buf,
                                 int32_t name_buf_len,
                                 uint8_t *lang_buf,
                                 int32_t lang_buf_len);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* TTS_WRAPPER_H */
