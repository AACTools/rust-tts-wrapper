// Dev stub of the omnivoice.cpp shared library (ABI v4) — for LOCAL
// test/development of the omnivoice-local engine without building the
// real GGML runtime (GH #64).
//
// The language table mirrors the REAL runtime's shape (verified against
// omnivoice.cpp src/lang-map.h): ids are bare ISO 639-3 codes, NO
// regional ids (no "pt-br"/"zh-cn"), NO macrolanguage "ar" (only
// "arb"), and ov_language_name takes the same INDEX as ov_language_id.
//
// Build:
//   gcc -shared -fPIC -o build/libomnivoice.so omnivoice_stub.c
// Use:
//   OMNIVOICE_LIB=/path/to/this/dir cargo test --features omnivoice-local
//   OMNIVOICE_TEST_MODEL=/dev/null OMNIVOICE_TEST_CODEC=/dev/null \
//     cargo test --features omnivoice-local --lib gh64
#include <string.h>
#include <stdlib.h>
#include <stdio.h>

typedef struct ov_context { int dummy; } ov_context;
typedef struct { const float *samples; int n_samples; int sample_rate; int channels; } ov_audio;
typedef struct { int abi_version; const char *model_path; const char *codec_path; unsigned char use_fa, clamp_fp16; const char *device; } ov_init_params;
typedef struct {
    int abi_version; const char *text; const char *lang; const char *instruct;
    int T_override; float chunk_duration_sec, chunk_threshold_sec; unsigned char denoise, preprocess_prompt;
    int mg_num_step; float mg_guidance_scale, mg_t_shift, mg_layer_penalty_factor, mg_position_temperature, mg_class_temperature;
    unsigned long long mg_seed; const int *ref_audio_tokens; int ref_T; const float *ref_audio_24k;
    int ref_n_samples; const char *ref_text; const char *dump_dir;
    void *cancel; void *cancel_user_data; void *on_chunk; void *on_chunk_user_data; unsigned char postproc;
} ov_tts_params;

static const char *LANGS[] = {"en", "english", "fr", "de", "es", "pt", "zh", "nl", "pl", "hi", "arb"};
static const char *NAMES[] = {"English", "English", "French", "German", "Spanish",
                              "Portuguese", "Chinese", "Dutch", "Polish", "Hindi", "Standard Arabic"};
static const int N_LANGS = 11;
static char last_err[256] = {0};

void ov_init_default_params(ov_init_params *p) { memset(p, 0, sizeof(*p)); }
ov_context *ov_init(const ov_init_params *p) {
    if (!p || !p->model_path || !p->codec_path || !*p->model_path || !*p->codec_path) return NULL;
    return calloc(1, sizeof(ov_context));
}
void ov_free(ov_context *ctx) { free(ctx); }
const char *ov_last_error(void) { return last_err; }
void ov_tts_default_params(ov_tts_params *p) { memset(p, 0, sizeof(*p)); }

static int lang_known(const char *lang) {
    if (!lang || !*lang) return 1; // auto-detect
    for (int i = 0; i < N_LANGS; i++) if (strcmp(lang, LANGS[i]) == 0) return 1;
    return 0;
}

int ov_synthesize(ov_context *ctx, const ov_tts_params *params, ov_audio *out) {
    memset(out, 0, sizeof(*out));
    if (!ctx || !params || !params->text) { snprintf(last_err, sizeof last_err, "invalid params"); return -1; }
    if (!lang_known(params->lang)) {
        snprintf(last_err, sizeof last_err, "ov_synthesize: unknown language '%s'", params->lang ? params->lang : "");
        return -1;
    }
    // ~1 second of 24 kHz silence.
    int n = 24000;
    float *buf = malloc((size_t)n * sizeof(float));
    for (int i = 0; i < n; i++) buf[i] = 0.0f;
    out->samples = buf; out->n_samples = n; out->sample_rate = 24000; out->channels = 1;
    last_err[0] = 0;
    return 0;
}

void ov_audio_free(ov_audio *a) {
    if (a && a->samples) { free((void *)a->samples); a->samples = NULL; a->n_samples = 0; }
}

int ov_n_languages(void) { return N_LANGS; }
const char *ov_language_id(int index) {
    if (index < 0 || index >= N_LANGS) return NULL;
    return LANGS[index];
}
// Real signature: takes the same INDEX as ov_language_id.
const char *ov_language_name(int index) {
    if (index < 0 || index >= N_LANGS) return NULL;
    return NAMES[index];
}
int ov_duration_sec_to_tokens(const ov_context *ctx, float seconds) {
    (void)ctx;
    if (seconds <= 0) return -1;
    return (int)(seconds * 75.0f); // 25 fps tokens x3 (arbitrary stub)
}
