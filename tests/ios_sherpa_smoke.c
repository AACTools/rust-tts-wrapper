// Link against the static iOS wrapper and check that it exposes Sherpa-ONNX.
#include "tts_wrapper.h"

#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char *argv[]) {
    struct tts_engine_info *engines = NULL;
    int32_t count = 0;
    if (tts_get_engines(&engines, &count) != 0) {
        fputs("Could not list TTS engines\n", stderr);
        return 1;
    }

    bool found = false;
    for (int32_t i = 0; i < count; i++) {
        found |= strcmp(engines[i].id, "sherpaonnx") == 0;
    }
    tts_free_engines(engines, count);
    if (!found) {
        fputs("Sherpa-ONNX is missing from the TTS engine list\n", stderr);
        return 1;
    }
    puts("Sherpa-ONNX is registered");

    if (argc < 2) {
        return 0;
    }

    // Pass a directory containing mms_eng/model.onnx and mms_eng/tokens.txt.
    if (chdir(argv[1]) != 0) {
        perror(argv[1]);
        return 1;
    }
    struct tts_ctx *client = tts_create(
        "sherpaonnx", "{\"modelPath\":\".\",\"modelId\":\"mms_eng\"}");
    if (!client) {
        fprintf(stderr, "Could not initialize Sherpa-ONNX: %s\n", tts_get_last_error(NULL));
        return 1;
    }
    uint8_t *bytes = NULL;
    size_t length = 0;
    int result = tts_synth_to_bytes(client, "Hello from the iOS simulator.", &bytes, &length);
    if (result != 0) {
        fprintf(stderr, "Sherpa-ONNX synthesis failed: %s\n", tts_get_last_error(client));
    }
    tts_free_bytes(bytes, length);
    tts_destroy(client);
    if (result != 0 || length == 0) {
        return 1;
    }
    printf("Sherpa-ONNX synthesized %zu bytes\n", length);
    return 0;
}
