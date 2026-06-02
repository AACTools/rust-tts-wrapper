#ifndef TTS_WRAPPER_H
#define TTS_WRAPPER_H

/* Auto-generated. Do not edit. */

#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

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
 * Write engine descriptors into a caller-allocated array.
 *
 * `out_engines` must point to at least [`tts_get_engine_count`] entries.
 * Caller must free each entry's strings and the array with [`tts_free_engine_info`].
 *
 * # Safety
 *
 * `out_engines` must be non-null and point to enough space.
 */
void tts_get_engines(struct tts_engine_info *out_engines);

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
 * The returned pointer is valid until the next call to any TTS function.
 */
const char *tts_get_last_error(void);

#ifdef __cplusplus
}
#endif

#endif  /* TTS_WRAPPER_H */
