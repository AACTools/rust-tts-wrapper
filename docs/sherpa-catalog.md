# The Sherpa catalog: readiness values and `catalogVersion`

`tts_get_sherpa_models_json` returns the embedded registry enriched
with two wrapper-computed fields per model — `g2p_enabled` (bool) and
`readiness` (string) — plus a top-level `catalogVersion` (integer).
Download surfaces and catalog UIs branch on these strings; this page
is the contract.

## `catalogVersion`

Bumped whenever the readiness **vocabulary** changes (a value renamed
or introduced) or a release flips values wholesale (wave releases do).

| Version | When | What changed |
|---|---|---|
| 1 | through 1.13.x | initial enrichment (`g2p_enabled`, `readiness`) |
| 2 | 1.14.0 | wave-3 locale flips: 78 models across 11 locales moved `blocked:bundle-not-validated` → `needs_bundle` |

Consumers should treat an unknown `catalogVersion` as "re-audit your
readiness switch"; values below document every state exhaustively.

## The `readiness` vocabulary

### Downloadable and speaks

| Value | Meaning | Consumer action |
|---|---|---|
| `ready` | checksummed archive, self-contained front-end | download → speak |
| `needs_bundle` | checksummed archive; the installer provisions a VoiceGarden G2P bundle after download (English-orphaned families and validated non-English vits locales) | download → provision → speak |
| `unverified` | works, but the registry row carries no sha256 (all MMS) | download at your own pinning; Dasher installs MMS via its own pinned path |
| `needs_assets:vocoder` | matcha family: a shared vocoder is fetched alongside the archive | download → fetch assets → speak |
| `needs_assets:vocoder-reference` | zipvoice family: cloning reference audio | download → resolve reference → speak |

### Not installable (the `blocked:` prefix)

| Value | Meaning | Consumer action |
|---|---|---|
| `blocked:no-url` | registry row has no download URL | nothing (registry bug) |
| `blocked:no-sha256` | no digest → not verifiably installable | nothing (registry gap) |
| `blocked:deprecated` | upstream archived / voice builder gone | hide from new installs |
| `blocked:g2p-unavailable` | the data is validated but THIS build lacks the `sherpa-g2p` feature | a rebuild fixes it, not new data |
| `blocked:no-language` | registry row carries no language | nothing (registry bug) |
| `blocked:bundle-not-validated` | a VoiceGarden bundle exists for the locale but no validated allowlist row | show as "coming"; probe campaigns flip these (see #92's waves) |
| `blocked:no-bundle` | no phonetic data exists for the locale anywhere | long-term blocked (ku/tn class) |

## Release discipline for catalog flips

Any release that changes readiness values for models must (per
#132):

1. bump `CATALOG_VERSION` (src/sherpa_catalog.rs) and add a row to
   the table above,
2. name the affected models/locales in the release notes (wave
   releases already summarize locale flips — extend that to the
   readiness dimension).

The catalog tests (`sherpa_catalog::tests::readiness_*`) pin the
vocabulary: adding or renaming a value fails locally until the doc
table and version are updated too.
