//! #82 Phase 4 — the German bundle bake-off: `de` (base) vs `deu`
//! (expanded) on the same words, with dictionary-vs-WFST attribution.
//!
//! The plan's acceptance gate: "run the Phase 4 word set through BOTH
//! bundles, score dictionary hits vs WFST guesses, and pick the
//! default from data". Prints the report and the expected-error
//! arithmetic behind the recommendation:
//!
//! ```text
//! expected mispronunciation ≈ WFST_share × (1 − WFST_holdout_exact)
//! ```
//!
//! (WFST holdout exact-match is per-manifest: de 93.6%, deu 86.1% —
//! the distilled additions in deu are unaudited, so its dictionary
//! hits are treated as equal-quality and only the WFST tier differs.)
//!
//! ```text
//! cargo run --release --features sherpaonnx,sherpa-g2p --example sherpa_g2p_bakeoff -- \
//!   <thorsten-tokens.txt> <de-dir> <deu-dir> [corpus.txt]
//! ```
//!
//! `<corpus.txt>`: `word count` per line (a German frequency list
//! works — the subtitle-derived `de_full` list was used for the
//! recorded bake-off). Without it, only the fixed Phase 4 word set
//! runs.
//! Tier meanings (drop/speak decided by the RUNTIME front-end —
//! `G2pFrontend::phonemize`, the exact production path): `dict` =
//! spoken from a dictionary hit · `wfst` = spoken via the fallback
//! chain (WFST guess, or rule fallback for WFST misses) · `drop` =
//! the runtime path dropped the word (both tiers unmappable).

use std::path::{Path, PathBuf};

use floravox_g2p_fst::MmapLexicon;
use rust_tts_wrapper::sherpa_g2p::{token_parts, G2pFrontend};

/// The plan's Phase 4 word set: common words, inflections, a compound,
/// proper names, number words the digit normalizer produces, and nonce
/// OOV words (the WFST must speak those, not drop them).
const WORD_SET: &[&str] = &[
    // common
    "guten",
    "morgen",
    "danke",
    "bitte",
    "haus",
    "wasser",
    "buch",
    // inflections
    "häuser",
    "liefen",
    "kindes",
    "autos",
    "gesehen",
    "gesagt",
    // compound
    "donaudampfschifffahrt",
    // names
    "karl",
    "müller",
    "schmidt",
    "anna",
    "berlin",
    "hamburg",
    // number words (digit-normalizer output shapes)
    "zweiundvierzig",
    "einskommafünf",
    "einemillion",
    // nonce OOV — the WFST must speak these
    "blorpinxel",
    "knarzwurst",
    "schlimmerding",
];

/// One bundle under test. The DROP/SPEAK decision comes from
/// `G2pFrontend::phonemize` — the exact runtime path (dictionary →
/// vocabulary ladder → lazy WFST → rule fallback → drop-if-unmappable),
/// per the Mac-team review; `MmapLexicon` is opened only to ATTRIBUTE
/// each spoken word to its tier (dictionary hit vs fallback) for the
/// report. The digit normalizer is part of that path, so digit-bearing
/// corpus tokens are measured as runtime treats them.
struct Bundle {
    frontend: G2pFrontend,
    lex: MmapLexicon,
}

#[derive(Default, Clone, Copy)]
struct Tally {
    dict: u64,
    wfst: u64,
    dropped: u64,
}

impl Tally {
    fn total(&self) -> u64 {
        self.dict + self.wfst + self.dropped
    }
    fn add(&mut self, tier: &str) {
        match tier {
            "dict" => self.dict += 1,
            "wfst" => self.wfst += 1,
            _ => self.dropped += 1,
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(tokens), Some(base_dir), Some(expanded_dir)) = (
        args.next().map(PathBuf::from),
        args.next().map(PathBuf::from),
        args.next().map(PathBuf::from),
    ) else {
        eprintln!("usage: sherpa_g2p_bakeoff <tokens.txt> <de-dir> <deu-dir> [corpus.txt]");
        std::process::exit(2);
    };
    let corpus = args.next().map(PathBuf::from);

    // one mapper per bundle; classify() needs &mut for memoization, so
    // each bundle's mapper lives behind its own mutable slot
    let mut bundles = [
        load("de", &base_dir, &tokens),
        load("deu", &expanded_dir, &tokens),
    ];

    println!("═══ Phase 4 word set ({} words)", WORD_SET.len());
    println!("{:<26}{:>8}{:>8}", "word", "de", "deu");
    let mut set_tally = [Tally::default(); 2];
    for word in WORD_SET {
        let mut tiers = Vec::new();
        for (i, b) in bundles.iter_mut().enumerate() {
            let tier = classify(b, word);
            set_tally[i].add(tier);
            tiers.push(tier);
        }
        println!("{word:<26}{:>8}{:>8}", tiers[0], tiers[1]);
    }
    report("word set", &set_tally);

    if let Some(corpus) = &corpus {
        println!(
            "\n═══ corpus OOV split ({} — top 50k types, frequency-weighted)",
            corpus.display()
        );
        let mut corpus_tally = [Tally::default(); 2];
        for line in std::fs::read_to_string(corpus)
            .expect("corpus readable")
            .lines()
            .take(50_000)
        {
            let Some((word, count)) = line.rsplit_once(' ') else {
                continue;
            };
            let Ok(weight) = count.parse::<u64>() else {
                continue;
            };
            for (i, b) in bundles.iter_mut().enumerate() {
                let tier = classify(b, word);
                // weighted tally without per-token loops: add in chunks
                let mut left = weight;
                while left > 0 {
                    let chunk = left.min(1_000_000);
                    for _ in 0..chunk {
                        corpus_tally[i].add(tier);
                    }
                    left -= chunk;
                }
            }
        }
        report("corpus (weighted)", &corpus_tally);
        verdict("de", 0.936, &corpus_tally[0]);
        verdict("deu", 0.861, &corpus_tally[1]);
    }
}

fn load(name: &'static str, dir: &Path, tokens: &Path) -> Bundle {
    let key = dir.file_name().unwrap().to_string_lossy().to_string();
    let _ = name; // used only in panic messages above
    let frontend =
        G2pFrontend::open(dir, &key, tokens).unwrap_or_else(|e| panic!("{name} frontend: {e}"));
    let lex = MmapLexicon::open(dir.join(format!("{key}.fst")))
        .unwrap_or_else(|e| panic!("{name} lexicon: {e}"));
    Bundle { frontend, lex }
}

/// Tier attribution + the RUNTIME decision for one word. Attribution
/// uses [`token_parts`] — the SAME normalized lookup words the runtime
/// phonemizes — so a digit-bearing corpus token counts as `dict` when
/// its NORMALIZED form (`42.` → `zweiundvierzig`) is a dictionary hit
/// (Mac-team review). `dict` = every part a dictionary hit · `wfst` =
/// at least one part resolved by the fallback chain (WFST guess or
/// rule spelling — the bucket is mixed) · `drop` = the runtime path
/// dropped it.
fn classify(b: &mut Bundle, word: &str) -> &'static str {
    let tier = match token_parts(word) {
        None => "drop",
        Some(parts) => {
            if parts
                .iter()
                .all(|p| b.lex.lookup(&p.to_lowercase()).is_some())
            {
                "dict"
            } else {
                "wfst"
            }
        }
    };
    let r = b.frontend.phonemize(word);
    if r.dropped == 0 && !r.sentences.is_empty() {
        tier
    } else {
        "drop"
    }
}

#[allow(clippy::cast_precision_loss)]
fn report(label: &str, t: &[Tally; 2]) {
    for (name, row) in [("de", &t[0]), ("deu", &t[1])] {
        let total = row.total().max(1);
        println!(
            "{label:<20} {name}: dict {:>6.2}%  wfst {:>6.2}%  drop {:>6.3}%",
            row.dict as f64 / total as f64 * 100.0,
            row.wfst as f64 / total as f64 * 100.0,
            row.dropped as f64 / total as f64 * 100.0,
        );
    }
}

#[allow(clippy::cast_precision_loss)]
/// A HEURISTIC, not a measurement (Mac-team review): the fallback
/// bucket mixes WFST guesses with rule-fallback spellings, and the
/// WFST holdout error rate only measures the WFST part — treat the
/// product as a rough comparison signal between bundles on the same
/// corpus, not an absolute quality figure.
fn verdict(name: &str, wfst_exact: f64, t: &Tally) {
    let total = t.total().max(1);
    let share = t.wfst as f64 / total as f64 * 100.0;
    let heuristic = share / 100.0 * (1.0 - wfst_exact) * 100.0;
    println!(
        "{name}: fallback share {share:.2}% × WFST holdout error {:.1}% → \
         heuristic error signal ≈ {heuristic:.2}% of tokens (heuristic: the \
         bucket mixes WFST guesses with rule spellings; compare across \
         bundles, not as an absolute)",
        (1.0 - wfst_exact) * 100.0
    );
}
