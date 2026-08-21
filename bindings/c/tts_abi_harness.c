/*
 * C ABI acceptance harness for rust-tts-wrapper.
 *
 * Compiles against the cbindgen header with -Wall -Wextra -Werror (the
 * header must be clean C) and links the cdylib, then exercises the ABI
 * the way the language bindings do:
 *
 *   engine enumeration → create (cloud engine, dummy creds, offline) →
 *   setters → all callback registrations → speak/speak_ssml/speak_sync
 *   (must fail cleanly with a dummy key, never crash) → synth_to_bytes
 *   error path → last_error → free_* → destroy.
 *
 * Exit code 0 = the C ABI contract holds. Any assertion failure exits 1
 * with a message on stderr.
 *
 * Build & run: see Makefile (or bindings/README.md).
 */

#include "tts_wrapper.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CHECK(cond, msg)                                                       \
    do {                                                                       \
        if (!(cond)) {                                                         \
            fprintf(stderr, "FAIL: %s (line %d)\n", msg, __LINE__);            \
            exit(1);                                                           \
        }                                                                      \
    } while (0)

/* Callback sinks: signatures must match the header typedefs exactly. */
static int audio_calls = 0;
static void on_audio(const uint8_t *data, uintptr_t len, void *userdata) {
    (void)data;
    (void)len;
    (void)userdata;
    audio_calls++;
}

static int boundary_calls = 0;
static void on_boundary(const char *word, int32_t char_offset, int32_t char_len,
                        float start_s, float end_s, int32_t estimated,
                        void *userdata) {
    (void)word;
    (void)char_offset;
    (void)char_len;
    (void)start_s;
    (void)end_s;
    (void)estimated;
    (void)userdata;
    boundary_calls++;
}

static int mark_calls = 0;
static void on_mark(const char *name, int32_t char_offset, float start_s,
                    float end_s, void *userdata) {
    (void)name;
    (void)char_offset;
    (void)start_s;
    (void)end_s;
    (void)userdata;
    mark_calls++;
}

static int viseme_calls = 0;
static void on_viseme(int32_t viseme_id, float offset_s, void *userdata) {
    (void)viseme_id;
    (void)offset_s;
    (void)userdata;
    viseme_calls++;
}

static int start_calls = 0;
static void on_start(void *userdata) {
    (void)userdata;
    start_calls++;
}

static int end_calls = 0;
static void on_end(void *userdata) {
    (void)userdata;
    end_calls++;
}

static int error_calls = 0;
static void on_error(const char *message, void *userdata) {
    (void)message;
    (void)userdata;
    error_calls++;
}

static void check_engines(void) {
    int32_t count = tts_get_engine_count();
    CHECK(count > 0, "engine count must be positive");

    tts_engine_info *engines = NULL;
    int32_t listed = 0;
    int32_t rc = tts_get_engines(&engines, &listed);
    CHECK(rc == 0, "tts_get_engines must succeed");
    CHECK(engines != NULL, "tts_get_engines must return an array");
    CHECK(listed == count, "listed engines must match engine count");
    for (int32_t i = 0; i < listed; i++) {
        CHECK(engines[i].id != NULL, "engine id must be non-null");
        CHECK(engines[i].name != NULL, "engine name must be non-null");
    }
    tts_free_engines(engines, listed);

    /* Hardening: null out-pointers return an error, not a crash. */
    CHECK(tts_get_engines(NULL, NULL) != 0, "null out-args must return error");
    tts_free_engines(NULL, 0);
}

static tts_ctx *check_create(void) {
    tts_ctx *ctx = tts_create("openai", "{\"apiKey\":\"dummy-key-for-c-harness\"}");
    CHECK(ctx != NULL, "tts_create(openai) must succeed offline");
    return ctx;
}

static void check_setters(tts_ctx *ctx) {
    tts_set_voice(ctx, "alloy");
    tts_set_voice(ctx, "");   /* empty is accepted */
    tts_set_voice(ctx, NULL); /* null is a no-op */
    tts_set_rate(ctx, 1.5f);
    tts_set_pitch(ctx, 0.8f);
    tts_set_volume(ctx, 0.9f);

    /* Null ctx is accepted as a no-op for every setter. */
    tts_set_voice(NULL, "alloy");
    tts_set_rate(NULL, 1.0f);
    tts_set_pitch(NULL, 1.0f);
    tts_set_volume(NULL, 1.0f);
}

static void check_callbacks(tts_ctx *ctx) {
    tts_set_on_audio(ctx, on_audio, NULL);
    tts_set_on_boundary(ctx, on_boundary, NULL);
    tts_set_on_mark(ctx, on_mark, NULL);
    tts_set_on_viseme(ctx, on_viseme, NULL);
    tts_set_on_start(ctx, on_start, NULL);
    tts_set_on_end(ctx, on_end, NULL);
    tts_set_on_error(ctx, on_error, NULL);

    /* Clear + re-register must be silent no-ops. */
    tts_set_on_boundary(ctx, NULL, NULL);
    tts_set_on_boundary(ctx, on_boundary, NULL);

    /* Null ctx accepted. */
    tts_set_on_audio(NULL, on_audio, NULL);
    tts_set_on_boundary(NULL, on_boundary, NULL);
    tts_set_on_mark(NULL, on_mark, NULL);
    tts_set_on_viseme(NULL, on_viseme, NULL);
    tts_set_on_start(NULL, on_start, NULL);
    tts_set_on_end(NULL, on_end, NULL);
    tts_set_on_error(NULL, on_error, NULL);
}

static void check_synth_failure_surface(tts_ctx *ctx) {
    /* All three speak entry points must fail cleanly (dummy key), not
     * crash; the offline contract mirrors the Rust conformance suite. */
    CHECK(tts_speak(ctx, "hello c abi") != 0, "tts_speak must fail offline");
    CHECK(tts_speak_ssml(ctx, "<speak>hello <mark name=\"m\"/></speak>") != 0,
          "tts_speak_ssml must fail offline");
    CHECK(tts_speak_sync(ctx, "hello c abi") != 0, "tts_speak_sync must fail offline");

    /* null text → error, not crash */
    CHECK(tts_speak(ctx, NULL) != 0, "null text must return error");
    CHECK(tts_speak_ssml(ctx, NULL) != 0, "null ssml must return error");
    CHECK(tts_speak_sync(ctx, NULL) != 0, "null text (sync) must return error");
    CHECK(tts_speak(NULL, "x") != 0, "null ctx must return error");

    uint8_t *bytes = NULL;
    uintptr_t len = 0;
    CHECK(tts_synth_to_bytes(ctx, "hello c abi", &bytes, &len) != 0,
          "synth_to_bytes must fail offline");
    CHECK(bytes == NULL, "no buffer handed out on failure");
    CHECK(len == 0, "length is zero on failure");
    CHECK(tts_synth_to_bytes(ctx, NULL, &bytes, &len) != 0,
          "null text (synth) must return error");

    /* The failed synth must populate last_error with a real message. */
    const char *err = tts_get_last_error(ctx);
    CHECK(err != NULL, "last_error must be populated after failure");
    CHECK(strlen(err) > 0, "last_error must be non-empty");

    tts_free_bytes(NULL, 0);
}

static void check_voices(tts_ctx *ctx) {
    tts_voice *voices = NULL;
    int32_t count = -1;
    int32_t rc = tts_get_voices(ctx, &voices, &count);
    CHECK(rc == 0, "get_voices must succeed (empty is fine)");
    /* openai offline: network-free construction yields zero voices. */
    CHECK(count >= 0, "voice count must not be negative");
    if (count > 0) {
        CHECK(voices != NULL, "voice array must be non-null");
        for (int32_t i = 0; i < count; i++) {
            CHECK(voices[i].id != NULL, "voice id must be non-null");
        }
    }
    tts_free_voices(voices, count);
    tts_free_voices(NULL, 0);
    CHECK(tts_get_voices(ctx, NULL, NULL) != 0,
          "null out-args must return error (voices)");
}

static void check_playback_control(tts_ctx *ctx) {
    /* Safe on an idle context: accepted no-ops. */
    tts_stop(ctx);
    tts_pause(ctx);
    tts_resume(ctx);
    tts_stop(NULL);
    tts_pause(NULL);
    tts_resume(NULL);
}

int main(void) {
    check_engines();

    tts_ctx *ctx = check_create();
    check_setters(ctx);
    check_callbacks(ctx);
    check_playback_control(ctx);
    check_voices(ctx);
    check_synth_failure_surface(ctx);

    tts_destroy(ctx);
    tts_destroy(NULL);

    printf("C ABI harness: OK (%d audio, %d boundary, %d mark, %d viseme, "
           "%d start, %d end, %d error callbacks observed)\n",
           audio_calls, boundary_calls, mark_calls, viseme_calls, start_calls,
           end_calls, error_calls);
    return 0;
}
