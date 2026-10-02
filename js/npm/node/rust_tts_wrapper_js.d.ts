/* tslint:disable */
/* eslint-disable */

/**
 * Synthesize through a cloud provider. `request_json` is a serialized
 * [`CloudRequest`]; `text` may be plain text, SSML, or SpeechMarkdown
 * (detected and compiled per provider, mirroring the native pipeline).
 */
export function cloud_speak(request_json: string, text: string): Promise<any>;

/**
 * Per-id durations, in latent frames.
 */
export function durations(handle: number, ids: BigInt64Array): Float32Array;

/**
 * Number of dictionary entries in a loaded language.
 */
export function g2p_lexicon_size(lang: number): number;

/**
 * Load a language: `tsv` = the bundle's lexicon.txt contents,
 * `oov_fst` = the bundle's phonetisaurus.fst bytes (optional, embedded tables).
 */
export function g2p_load_lang(tsv: string, oov_fst?: Uint8Array | null): number;

/**
 * phonemize one word through the chain (dictionary -> WFST).
 * Returns IPA symbols joined by spaces, or null when both tiers miss
 * (the JS side then falls back to ByT5).
 */
export function g2p_word(lang: number, word: string): string | undefined;

/**
 * Load a `.student` file. `bytes` is the raw file contents.
 * Returns a handle for later calls.
 */
export function load_student(bytes: Uint8Array): number;

/**
 * Spike: load the duration-student ONNX from bytes, run it on i64 ids,
 * return the predicted durations (frames per token, float).
 */
export function onnx_duration_run(model: Uint8Array, ids: BigInt64Array): Promise<Float32Array>;

/**
 * Call once before any other onnx fn: injects the JS-backed OrtApi.
 * Serves onnxruntime-web from the jsdelivr CDN by default.
 */
export function onnx_init(): Promise<void>;

/**
 * Parse SpeechMarkdown: returns `{ ssml, segments, plain }`.
 * `segments` is the document-order timeline for the editor preview.
 */
export function smd_parse(input: string): any;

/**
 * The one entry point: speak `text` (plain, SSML, or SpeechMarkdown —
 * each engine compiles it per its dialect) through the configured engine.
 * Returns { audio: Float32Array|Uint8Array, sr, spans, marks } for
 * floravox or { audio: Uint8Array, mime } for cloud.
 */
export function speak(config_json: string, text: string): Promise<any>;

/**
 * Load the three-student ONNX trio. `sr`/`hop` come from the voice
 * config (22050/256 for the piper-derived students).
 */
export function student_stack_load(duration: Uint8Array, acoustic: Uint8Array, decoder: Uint8Array, sr: number, hop: number): Promise<number>;

/**
 * Synthesize interleaved ids through the student trio.
 * Returns `{ audio: Vec<f32>, sr, durations: Vec<i64> }` where durations
 * are the merged per-phoneme frames (token + trailing pad) for word
 * timing, same convention as the JS path.
 */
export function student_stack_run(handle: number, ids: BigInt64Array, length_scale: number): Promise<any>;

/**
 * Load a piper teacher ONNX (original 2-output or patched 4-output).
 * `voice_json` is the voice's .onnx.json (phoneme map + audio config).
 * Returns a handle. Call `onnx_init` once first.
 */
export function teacher_load(model: Uint8Array, voice_json: string): Promise<number>;

/**
 * Synthesize interleaved ids: `scales` = [noise, length, noise_w] (piper
 * order). Returns Float32Array audio at the voice's sample rate plus
 * per-position durations when the graph exposes them.
 */
export function teacher_run(handle: number, ids: BigInt64Array, scales: Float32Array): Promise<any>;

/**
 * Sample rate / hop of a loaded student, as [sr, hop, calibration].
 */
export function timing_meta(handle: number): Float32Array;

/**
 * Load a student voice for orchestration: `student` is a handle from
 * `student_stack_load`, `lang` a g2p handle from `g2p_load_lang`,
 * `phoneme_id_map_json` the voice config's `phoneme_id_map` object.
 */
export function voice_load_student(student: number, lang: number, phoneme_id_map_json: string, sr: number, hop: number): number;

/**
 * Speak SSML (or plain text) through a loaded student voice. Breaks become
 * silence; marks fire at the boundary sample; per-segment prosody rate
 * scales `length_scale`. SpeechMarkdown callers compile to SSML first
 * (`smd_parse`).
 */
export function voice_speak(handle: number, input: string): Promise<any>;
