#import <Foundation/Foundation.h>
#import <AVFAudio/AVSpeechSynthesis.h>

void* avsynth_create(void) {
    @try {
        AVSpeechSynthesizer *synth = [[AVSpeechSynthesizer alloc] init];
        return (void *)CFBridgingRetain(synth);
    } @catch (NSException *e) {
        return NULL;
    }
}

void avsynth_destroy(void *handle) {
    if (!handle) return;
    @try {
        CFRelease(handle);
    } @catch (NSException *e) {}
}

void avsynth_speak(void *handle, const char *text, const char *voice_id,
                   float rate, float pitch, float volume) {
    if (!handle || !text) return;
    @try {
        AVSpeechSynthesizer *synth = (__bridge AVSpeechSynthesizer *)handle;
        NSString *nsText = [NSString stringWithUTF8String:text];
        if (!nsText) return;
        AVSpeechUtterance *utterance = [[AVSpeechUtterance alloc] initWithString:nsText];
        utterance.rate = rate < 0.1f ? 0.1f : (rate > 10.0f ? 10.0f : rate);
        utterance.pitchMultiplier = pitch < 0.5f ? 0.5f : (pitch > 2.0f ? 2.0f : pitch);
        utterance.volume = volume < 0.0f ? 0.0f : (volume > 1.0f ? 1.0f : volume);
        if (voice_id && voice_id[0] != '\0') {
            NSString *nsVoiceId = [NSString stringWithUTF8String:voice_id];
            AVSpeechSynthesisVoice *voice = [AVSpeechSynthesisVoice voiceWithIdentifier:nsVoiceId];
            if (voice) utterance.voice = voice;
        }
        [synth speakUtterance:utterance];
    } @catch (NSException *e) {}
}

void avsynth_stop(void *handle) {
    if (!handle) return;
    @try {
        AVSpeechSynthesizer *synth = (__bridge AVSpeechSynthesizer *)handle;
        [synth stopSpeakingAtBoundary:AVSpeechBoundaryImmediate];
    } @catch (NSException *e) {}
}

void avsynth_pause(void *handle) {
    if (!handle) return;
    @try {
        AVSpeechSynthesizer *synth = (__bridge AVSpeechSynthesizer *)handle;
        [synth pauseSpeakingAtBoundary:AVSpeechBoundaryImmediate];
    } @catch (NSException *e) {}
}

void avsynth_resume(void *handle) {
    if (!handle) return;
    @try {
        AVSpeechSynthesizer *synth = (__bridge AVSpeechSynthesizer *)handle;
        [synth continueSpeaking];
    } @catch (NSException *e) {}
}

int avsynth_voice_count(void *handle) {
    if (!handle) return 0;
    @try {
        NSArray<AVSpeechSynthesisVoice *> *voices = [AVSpeechSynthesisVoice speechVoices];
        return (int)[voices count];
    } @catch (NSException *e) {
        return 0;
    }
}

int avsynth_get_voice(void *handle, int index,
                      char *id_buf, int id_buf_len,
                      char *name_buf, int name_buf_len,
                      char *lang_buf, int lang_buf_len) {
    if (!handle) return -1;
    @try {
        NSArray<AVSpeechSynthesisVoice *> *voices = [AVSpeechSynthesisVoice speechVoices];
        if (index < 0 || index >= (int)[voices count]) return -1;
        AVSpeechSynthesisVoice *voice = voices[index];

        NSString *identifier = voice.identifier ?: @"";
        NSString *name = voice.name ?: @"";
        NSString *language = voice.language ?: @"";

        strncpy(id_buf, [identifier UTF8String], id_buf_len - 1);
        id_buf[id_buf_len - 1] = '\0';
        strncpy(name_buf, [name UTF8String], name_buf_len - 1);
        name_buf[name_buf_len - 1] = '\0';
        strncpy(lang_buf, [language UTF8String], lang_buf_len - 1);
        lang_buf[lang_buf_len - 1] = '\0';
        return 0;
    } @catch (NSException *e) {
        return -1;
    }
}
