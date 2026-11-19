# User pronunciation overrides

A plain-text file where you teach the wrapper how words should sound,
applied automatically before synthesis on **every engine**. Long-standing
Dasher request (issue #125): the voice mispronounces a word — often a
person's name or a medication — and the only workaround was typing the
respelling into the sentence itself.

## The file

Default location: `~/.rust-tts-wrapper/pronunciations.txt`
(`%USERPROFILE%\.rust-tts-wrapper\pronunciations.txt` on Windows).
Editable with Notepad; `#` lines are comments; one entry per line:

```text
# respell — universal fallback, works on every engine
botox = bow tocks

# phoneme override — precise IPA where the engine accepts phonemes
# (sherpa-onnx voices today)
botox = /ˈboʊ.tɑːks/

# multi-word phrases (names) work
nguyen = wen
san rafael = san rah fah el
```

Both forms for the same word may coexist (as above) — the engine picks
the best one it supports.

Frontends can point elsewhere (Dasher keeps the file under
`%APPDATA%\Dasher`):

- C ABI: `tts_set_pronunciations_file(path)` (NULL/empty = default)
- Environment: `RUST_TTS_PRONUNCIATIONS=/path/to/file`

Editor/frontend support ABI (#131):

- `tts_pronunciations_active()` — is a file live? badge the editor
  when false (a bindings pin older than the feature ignores the file)
- `tts_apply_pronunciations(text)` — dry-run: the text after respell
  substitutions, for "will be spoken as …" previews
- `tts_pronunciations_stats(&entries, &skipped)` — entry count +
  malformed-line count, to flag `botox =` typos in place
- Swift: `TtsClient.setPronunciationsFile(_:)/pronunciationsActive/
  applyPronunciations(_:/pronunciationsStats()` (#130)

A leading BOM (Notepad's default save) is stripped — the first entry
survives a Windows round-trip (#131).

Edits take effect on the **next utterance** (the file is re-read when
its modification time changes — no rebuild, no restart of the file
watching kind).

## Matching rules

- **Whole-word, case-insensitive**: `botox` matches `Botox`, `BOTOX`,
  `Botox,` and `(botox)` — but never `elbow` or `botoxic`.
- **Longest first**: `san rafael` wins over a bare `rafael` entry.
- Substitutions happen **before digit normalization**, so a respell
  containing digits (`chapter2 = chapter two`) still reads correctly.
- Keys and text match on single-space phrasing: a key `san rafael`
  does not match a double-spaced `san  rafael` in the text.

## Which form each engine uses

| Engine | Respell form | Phoneme form |
|---|---|---|
| sherpa-onnx, plain-text voices (English lexicon front-end) | ✅ | n/a (text in) |
| sherpa-onnx, G2P front-end (the non-English fleet) | ✅ | ✅ via the voice's own token vocabulary — any symbol the voice rejects falls back to the word's NORMAL pronunciation (dictionary/G2P), never silence |
| phoneme-only entries (no respell line) on any voice without the G2P front-end | unchanged (default pronunciation) | n/a — **add a respell line too** if you use these voices |
| Azure / Google / Polly / ElevenLabs / … (cloud) | ✅ | falls back to respell (SSML `<phoneme>` wrapping is a follow-up) |
| SAPI / system / avsynth | ✅ | falls back to respell |
| SSML passed via `tts_speak_ssml` | untouched (embed `<phoneme>` yourself) | untouched |

Word-boundary/highlight events are remapped: every spoken word of a
respell highlights the word the user **typed**.

## Non-goals

Heteronyms (`tear` = cry vs. rip) cannot be solved by a static file —
see issue #125 for the reasoning. This feature is whole-word overrides
only.
