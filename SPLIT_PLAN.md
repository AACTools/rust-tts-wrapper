# cloud_engine.rs module split — plan (not yet executed)

The one-shot scripted split of the 5,929-line file failed repeatedly:
brace-counting drifts on doc comments containing braces and on
multi-line fn signatures wrapped in parens; marker-based sectioning
breaks on line drift. Do it manually, compiler-driven, module by
module, in this order (verified section boundaries included).

## Verified section boundaries (line ranges on main @ 72aacc8)
engine_viseme 9..21, header_imports 21..45, decode 45..615,
edge 615..723, config_a 723..1101, ssml 1101..1348, google 1348..1453,
config_b 1453..1464, gemini 1464..1634, elevenlabs 1634..1743,
voices_a 1743..2110, engine_impl 2110..3138, voices_b 3138..3764,
engine_tail 3764..3774, tests 3774..5930.

## Steps
1. Create `src/cloud_engine/mod.rs` holding: the shared import block,
   the viseme bridge (VisemeFn/VISEME_CB/set_viseme_callback),
   STREAMING_CHUNK_SIZE, module declarations, glob re-exports
   (`pub(crate) use <mod>::*;`), and the public facade re-exports.
2. Move ONE section per commit, running `cargo test --features cloud`
   between each: decode → edge → config → ssml → google → gemini →
   elevenlabs → voices → engine impl → tests.
3. Visibility rules learned:
   - top-level items: pub(crate)
   - CloudConfig + CloudEngine fields: pub(crate)
   - inherent impl methods: pub(crate)
   - TRAIT impl methods: no visibility (E0449)
   - tests module must be a CHILD of engine.rs (#[path]) — private
     field access requires descendant scoping
4. Lexer pitfalls that broke scripted attempts:
   - count (), [], {} — not just braces (multi-line fn signatures!)
   - ignore // comments (braces in comments drift the count)
   - handle " and ' " " char literals (string-state flips)
5. CI: the clippy `-D warnings` gates flag wildcard-imports and
   unused-imports on any mistake — run the exact CI clippy commands
   locally per commit (system,cloud / avsynth,cloud / sapi,cloud).

## Also pending for the split PR
- Source-grep test (chunk-size) must read the split file list.
- gemini/elevenlabs buffered branches adopt the StreamMux delivery
  (prototype exists in git history: refactor/cloud-split-pump).
