// Dev stub of the qwen3-tts.cpp shared library (qwen3tts) — LOCAL
// development/testing of the qwen3-local feature without building the
// real GGML runtime. Symbols only; all functions fail softly.
// Build:  gcc -shared -fPIC -o build/libqwen3tts.so qwen3tts_stub.c
// Use:    QWEN3_TTS_LIB=/path/to/this/dir cargo check --features qwen3-local
#include <stdlib.h>
#include <string.h>

typedef struct { char _[8]; } Qwen3Tts;
typedef struct { char _[16]; } Qwen3TtsAudio;

void qwen3_tts_default_params(void *params) { memset(params, 0, 200); }
Qwen3Tts *qwen3_tts_create(const char *model_dir, int n_threads) {
    (void)n_threads;
    if (!model_dir || !*model_dir) return NULL;
    return calloc(1, sizeof(Qwen3Tts));
}
int qwen3_tts_is_loaded(const Qwen3Tts *tts) { return tts != NULL; }
int qwen3_tts_synthesize(Qwen3Tts *t, const void *p, Qwen3TtsAudio **out) {
    (void)t; (void)p; *out = NULL; return -1;
}
void qwen3_tts_free_audio(Qwen3TtsAudio *a) { free(a); }
void qwen3_tts_destroy(Qwen3Tts *t) { free(t); }
int qwen3_tts_synthesize_with_voice_file(Qwen3Tts *t, const void *p, const char *wav, Qwen3TtsAudio **out) {
    (void)t; (void)p; (void)wav; *out = NULL; return -1;
}
int qwen3_tts_extract_embedding_file(const char *wav, int *n, float **emb) {
    (void)wav; (void)n; (void)emb; return -1;
}
int qwen3_tts_synthesize_with_embedding(Qwen3Tts *t, const void *p, const float *e, int n, Qwen3TtsAudio **out) {
    (void)t; (void)p; (void)e; (void)n; *out = NULL; return -1;
}
const char *qwen3_tts_get_error(const Qwen3Tts *tts) {
    (void)tts;
    return "qwen3 stub: synthesis not implemented";
}
