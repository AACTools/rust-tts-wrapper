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

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly cloud_speak: (a: number, b: number, c: number, d: number) => any;
    readonly durations: (a: number, b: number, c: number) => [number, number, number, number];
    readonly tts_create: (a: number, b: number) => number;
    readonly tts_destroy: (a: number) => void;
    readonly tts_free_bytes: (a: number, b: number) => void;
    readonly tts_free_engines: (a: number, b: number) => void;
    readonly tts_free_voices: (a: number, b: number) => void;
    readonly tts_get_engine_count: () => number;
    readonly tts_get_engines: (a: number, b: number) => number;
    readonly tts_get_last_error: (a: number) => number;
    readonly tts_get_voices: (a: number, b: number, c: number) => number;
    readonly tts_pause: (a: number) => void;
    readonly tts_resume: (a: number) => void;
    readonly tts_set_on_audio: (a: number, b: number, c: number) => void;
    readonly tts_set_on_boundary: (a: number, b: number, c: number) => void;
    readonly tts_set_on_end: (a: number, b: number, c: number) => void;
    readonly tts_set_on_error: (a: number, b: number, c: number) => void;
    readonly tts_set_on_mark: (a: number, b: number, c: number) => void;
    readonly tts_set_on_start: (a: number, b: number, c: number) => void;
    readonly tts_set_on_viseme: (a: number, b: number, c: number) => void;
    readonly tts_set_pitch: (a: number, b: number) => void;
    readonly tts_set_rate: (a: number, b: number) => void;
    readonly tts_set_voice: (a: number, b: number) => void;
    readonly tts_set_volume: (a: number, b: number) => void;
    readonly tts_speak: (a: number, b: number) => number;
    readonly tts_speak_ssml: (a: number, b: number) => number;
    readonly tts_speak_sync: (a: number, b: number) => number;
    readonly tts_stop: (a: number) => void;
    readonly tts_synth_to_bytes: (a: number, b: number, c: number, d: number) => number;
    readonly g2p_lexicon_size: (a: number) => number;
    readonly g2p_load_lang: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly g2p_word: (a: number, b: number, c: number) => [number, number];
    readonly load_student: (a: number, b: number) => [number, number, number];
    readonly onnx_duration_run: (a: number, b: number, c: number, d: number) => any;
    readonly onnx_init: () => any;
    readonly smd_parse: (a: number, b: number) => [number, number, number];
    readonly speak: (a: number, b: number, c: number, d: number) => any;
    readonly student_stack_load: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => any;
    readonly student_stack_run: (a: number, b: number, c: number, d: number) => any;
    readonly teacher_load: (a: number, b: number, c: number, d: number) => any;
    readonly teacher_run: (a: number, b: number, c: number, d: number, e: number) => any;
    readonly timing_meta: (a: number) => [number, number, number, number];
    readonly voice_load_student: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number];
    readonly voice_speak: (a: number, b: number, c: number) => any;
    readonly speechmarkdown_free: (a: number) => void;
    readonly speechmarkdown_get_error: () => number;
    readonly speechmarkdown_is_speech_markdown: (a: number) => number;
    readonly speechmarkdown_parse: (a: number) => number;
    readonly speechmarkdown_supported_ssml: (a: number) => number;
    readonly speechmarkdown_to_smd: (a: number) => number;
    readonly speechmarkdown_to_ssml: (a: number, b: number) => number;
    readonly speechmarkdown_to_text: (a: number) => number;
    readonly speechmarkdown_validate: (a: number) => number;
    readonly wasm_bindgen_4b73c5b103bcba97___convert__closures_____invoke___js_sys_ee7a4206a7f869b5___Boolean__core_608f92abc48d28da___result__Result_____wasm_bindgen_4b73c5b103bcba97___JsError___true_: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_4b73c5b103bcba97___convert__closures_____invoke___js_sys_ee7a4206a7f869b5___Boolean__core_608f92abc48d28da___result__Result_____wasm_bindgen_4b73c5b103bcba97___JsError___true__20: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_4b73c5b103bcba97___convert__closures_____invoke___js_sys_ee7a4206a7f869b5___Function_fn_wasm_bindgen_4b73c5b103bcba97___JsValue_____wasm_bindgen_4b73c5b103bcba97___sys__Undefined___js_sys_ee7a4206a7f869b5___Function_fn_wasm_bindgen_4b73c5b103bcba97___JsValue_____wasm_bindgen_4b73c5b103bcba97___sys__Undefined_______true_: (a: number, b: number, c: any, d: any) => void;
    readonly wasm_bindgen_4b73c5b103bcba97___convert__closures_____invoke___wasm_bindgen_4b73c5b103bcba97___JsValue__core_608f92abc48d28da___result__Result_____wasm_bindgen_4b73c5b103bcba97___JsError___true_: (a: number, b: number, c: any) => [number, number];
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_destroy_closure: (a: number, b: number) => void;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
