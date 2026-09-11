/**
 * Node.js bindings for the rust-tts-wrapper C ABI.
 * See bindings/README.md for the library-loading conventions.
 */

export interface TtsVoice {
  id: string;
  name: string;
  language: string;
  gender: string;
  engine: string;
}

export interface TtsEngineInfo {
  id: string;
  name: string;
  needsCredentials: boolean;
  credentialKeys: string[];
}

export interface WordBoundaryEvent {
  word: string;
  charOffset: number;
  charLen: number;
  startSec: number;
  endSec: number;
  /** true = proportional estimate, false = measured timings */
  estimated: boolean;
}

export interface MarkEvent {
  name: string;
  charOffset: number;
  startSec: number;
  endSec: number;
}

export interface VisemeEvent {
  id: number;
  offsetSec: number;
}

export interface TtsClientOptions {
  engineId?: string;
  credentials?: Record<string, string>;
}

declare class TtsClient extends NodeJS.EventEmitter {
  constructor(options?: TtsClientOptions);

  speak(text: string): void;
  speakSsml(ssml: string): void;
  speakSync(text: string): void;
  synthToBytes(text: string): Buffer;

  stop(): void;
  pause(): void;
  resume(): void;

  setVoice(voiceId: string): void;
  setRate(rate: number): void;
  setPitch(pitch: number): void;
  setVolume(volume: number): void;

  getVoices(): TtsVoice[];
  lastError(): string | null;
  close(): void;

  on(event: "audio", listener: (chunk: Buffer) => void): this;
  on(event: "boundary", listener: (ev: WordBoundaryEvent) => void): this;
  on(event: "mark", listener: (ev: MarkEvent) => void): this;
  on(event: "viseme", listener: (ev: VisemeEvent) => void): this;
  on(event: "start" | "end", listener: () => void): this;
  on(event: "error", listener: (message: string) => void): this;

  static listEngines(): TtsEngineInfo[];
  static engineCount(): number;
  static globalLastError(): string | null;
}

/** Preload the shared library (throws with guidance if not found). */
export function loadLibrary(explicitPath?: string): unknown;

export declare const TtsClientConstructor: typeof TtsClient;
