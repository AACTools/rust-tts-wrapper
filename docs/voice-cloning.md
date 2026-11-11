# Voice cloning (experimental, `cloning` feature)

Bank a voice once, enroll it with every cloning-capable engine, speak
it everywhere. Rust-only for now (no FFI); enable with
`--features cloning`.

```rust
use rust_tts_wrapper::cloning::{create_cloner, CloneOutcome, VoiceCorpus};

// Import an Apple Personal Voice "Recordings" export (CAF/ALAC → PCM),
// or an LJSpeech corpus directory (metadata.csv + wav/).
let corpus = VoiceCorpus::from_personal_voice_zip("Will's Personal Voice 1 - Recordings.zip")?;
let identity = corpus.to_identity(Some("en"));

// Enroll: qwen + elevenlabs are instant; google/azure are consent-gated.
let cloner = create_cloner("qwen", r#"{"apiKey":"sk-..."}"#).ok_or("no cloner")?;
let CloneOutcome::Ready(handle) = cloner.clone_voice(&identity)? else { unreachable!() };

// Speak it — a cloned voice id is just a voice string.
let engine = rust_tts_wrapper::create_engine(
    "qwen", r#"{"apiKey":"sk-...","modelId":"qwen-audio-3.0-tts-flash"}"#)?;
engine.speak("Hello", Some(&handle.voice_id), 1.0, 1.0, 1.0,
    Some(&mut |_| {}), None, None)?;

cloner.delete_cloned(&handle)?; // quota hygiene
```

## Capture is source-agnostic

- `VoiceIdentityBuilder` records live microphone PCM clip-by-clip
  (`start_clip` / `push_pcm` / `finish_clip`).
- `VoiceIdentity::from_transcribed_pairs` takes audio files +
  transcripts.
- `AudioClip::from_audio_file` loads wav/mp3/m4a/flac;
  `VoiceCorpus::from_audio_dir` imports a folder.
- Apple Personal Voice and LJSpeech are just two importers.

## Persistence

`CloneRegistry` maps identity → engine handles
(`~/.rust-tts-wrapper/clones.json`, atomic saves):

```rust
use rust_tts_wrapper::cloning::{default_registry_path, CloneRegistry};

let mut registry = CloneRegistry::load(default_registry_path().unwrap())?;
registry.add(&identity.name, handle.clone());
registry.save()?;
let handles = registry.handles(&identity.name);
```

## Consent-gated engines (Google ICV, Azure Personal Voice)

These need a recording of the provider's fixed script first —
discoverable before asking the user to record:

```rust
let cloner = create_cloner("google", creds)?;
if let Some(spec) = cloner.consent_spec() {
    // record spec.script verbatim, then attach:
    identity.consent.push(ConsentRecording {
        engine: "google".into(),
        pcm,                       // PCM16 LE mono
        sample_rate: 24_000,
        metadata: [("language_code".to_string(), "en-US".to_string())].into(),
    });
}
```

Azure additionally requires `voiceTalentName`/`companyName`/`locale`
metadata and is the one job-mode engine: `clone_voice` may return
`CloneOutcome::Pending { job_id }` — resolve with
`cloner.poll_clone(&job_id)`. Neither is live-testable without vendor
approval; request shapes are doc-verified and unit-tested.

## Local cloning (fully offline)

**qwen3-local** (`--features qwen3-local,cloning`): extracts an ECAPA
speaker embedding from the identity's best clip; the `emb:` handle is
reused per utterance. No upload, no consent gate, no quota
(live-verified).

**pocket** (`pocket-timing`): clones from any donor wav — see the
[engines doc](engines.md#pocket-kyutai) for its honest caveats.

## Apple Personal Voice end-to-end

1. The user exports from Accessibility → Personal Voice (on-device,
   user-initiated): `"{Voice Name} - Recordings.zip"`.
2. Inside: `TrainingData/` with `{session5}_{NN}.caf` clips (48 kHz
   ALAC; ~150 clips / ~12 min on iOS 17, ~10 on iOS 26 — far more than
   any instant cloner needs).
3. `from_personal_voice_zip` decodes CAF/ALAC via symphonia (no
   ffmpeg), canonicalizes to PCM16 mono 24 kHz, takes the voice name
   from the zip title.
4. Transcripts are NOT in the zip — attach via
   `VoiceCorpus::phrases` or ASR if an engine needs them (Qwen and
   ElevenLabs ignore transcripts).

`examples/voice-clone.rs` runs the full flow; `tests/cloning_live.rs`
(`QWEN_API_KEY` + `QWEN_PV_ZIP`, `--ignored`) tests import → clone →
speak → delete against a real export.

## Ethics

Cloning someone's voice requires their permission; banked-voice
programs' licensed synthetic voices must not be re-cloned.
