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

/**
 * Stable error codes (GH #69) — append-only u32 values from the C ABI.
 * Runtime export is a frozen object (a const enum would break under
 * `isolatedModules`); compare with `===` against these values.
 */
export const TtsErrorCode: {
  readonly Unknown: 0;
  readonly MissingConfiguration: 1;
  readonly InvalidConfiguration: 2;
  readonly CredentialsRejected: 3;
  readonly NetworkError: 4;
  readonly ModelNotFound: 5;
  readonly MissingNativeDependency: 6;
  readonly EngineNotAvailable: 7;
  readonly LanguageNotSupported: 8;
  readonly EngineFailure: 9;
};

/** Structured error detail (GH #69): message is secret-redacted. */
export interface TtsErrorDetail {
  code: TtsErrorCode;
  codeName: string;
  message: string;
  displayMessage: string;
  engineId: string;
  /** Offending credential field (e.g. "subscriptionKey"), "" when n/a. */
  fieldKey: string;
}

/** Outcome of the tri-state credential check (GH #69). */
export type CredentialCheck =
  | { status: "verified" }
  | { status: "rejected" }
  | { status: "unverified" }
  | { status: "error"; error: TtsErrorDetail };

/** Control kind for a setup field (GH #68). */
export type SetupFieldKind =
  | "secret"
  | "filePath"
  | "directoryPath"
  | "url"
  | "choice"
  | "number"
  | "text";

/** One typed setup/credential field (GH #68). */
export interface TtsSetupField {
  key: string;
  label: string;
  kind: SetupFieldKind;
  required: boolean;
  defaultValue?: string;
  choices: string[];
  placeholder?: string;
  helpText?: string;
}

/** Engine capability flags (GH #68). */
export interface TtsEngineCapabilities {
  supportsRate: boolean;
  supportsPitch: boolean;
  supportsVolume: boolean;
  supportsWordTiming: boolean;
  supportsSsml: boolean;
  supportsStreaming: boolean;
  requiresInternet: boolean;
  /** Voice enumeration succeeds without authentication — listing
   * alone is not credential validation. */
  staticVoiceCatalog: boolean;
  notes?: string;
}

/** Full setup schema for one engine (GH #68). */
export interface TtsEngineSetup {
  id: string;
  name: string;
  needsCredentials: boolean;
  fields: TtsSetupField[];
  capabilities: TtsEngineCapabilities;
  /** Either/or groups: at least one field in each group is required. */
  requiredAny: string[][];
}

/** Error thrown by TtsClient operations; carries the stable code
 * (GH #69) when the native build provides it. */
export declare class TtsError extends Error {
  /** TtsErrorCode value (numeric) — see the TtsErrorCode export. */
  code: number;
  codeName: string;
  fieldKey: string;
  engineId: string;
  displayMessage: string | null;
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
  /** Tri-state credential check (GH #69): static catalogs answer
   * "unverified" — enumerating them proves nothing about the key. */
  checkCredentials(): CredentialCheck;
  lastError(): string | null;
  /** Stable classification of the last error (0 = none, GH #69). */
  lastErrorCode(): TtsErrorCode;
  /** Structured detail of the last error, or null (GH #69). */
  lastErrorDetail(): TtsErrorDetail | null;
  close(): void;

  on(event: "audio", listener: (chunk: Buffer) => void): this;
  on(event: "boundary", listener: (ev: WordBoundaryEvent) => void): this;
  on(event: "mark", listener: (ev: MarkEvent) => void): this;
  on(event: "viseme", listener: (ev: VisemeEvent) => void): this;
  on(event: "start" | "end", listener: () => void): this;
  on(event: "error", listener: (message: string) => void): this;

  static listEngines(): TtsEngineInfo[];
  static engineCount(): number;
  /** Typed setup schema for every engine compiled into this build
   * (GH #68): render credential rows without a provider switch. */
  static engineSetups(): TtsEngineSetup[];
  /** The setup schema for one engine id, or null when unknown. */
  static engineSetup(engineId: string): TtsEngineSetup | null;
  static globalLastError(): string | null;
  static globalLastErrorCode(): TtsErrorCode;
  static globalLastErrorDetail(): TtsErrorDetail | null;
}

/** Preload the shared library (throws with guidance if not found). */
export function loadLibrary(explicitPath?: string): unknown;

export declare const TtsClientConstructor: typeof TtsClient;
