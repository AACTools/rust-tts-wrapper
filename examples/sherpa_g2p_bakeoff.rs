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
//! `<corpus.txt>`: `word count` per line (a frequency list; the
//! OpenSubtitles [`de_full`] list works). Without it, only the fixed
//! Phase 4 word set runs.
//!
//! [`de_full`]: https://github.com/hermitdave/FrequencyWords/blob/master/content/2018/de/de_full.txt Tier meanings: `dict` = dictionary hit ·
//! `wfst` = WFST guess whose phones all map into the model vocabulary ·
//! `unmap` = WFST guess with phones the model cannot speak · `drop` =
//! no path.

use std::path::{Path, PathBuf};

use floravox_g2p_fst::{MmapLexicon, OovFallback, PhonetisaurusG2p};
use rust_tts_wrapper::sherpa_lexicon::VocabPhonemeMap;

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

struct Bundle {
    lex: MmapLexicon,
    wfst: Option<PhonetisaurusG2p>,
    mapper: VocabPhonemeMap,
}

#[derive(Default, Clone, Copy)]
struct Tally {
    dict: u64,
    wfst: u64,
    unmap: u64,
    dropped: u64,
}

impl Tally {
    fn total(&self) -> u64 {
        self.dict + self.wfst + self.unmap + self.dropped
    }
    fn add(&mut self, tier: &str) {
        match tier {
            "dict" => self.dict += 1,
            "wfst" => self.wfst += 1,
            "unmap" => self.unmap += 1,
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
    let lex = MmapLexicon::open(dir.join(format!("{key}.fst")))
        .unwrap_or_else(|e| panic!("{name} lexicon: {e}"));
    let wfst = dir
        .join("phonetisaurus.fst")
        .exists()
        .then(|| PhonetisaurusG2p::open(dir.join("phonetisaurus.fst")).expect("wfst"));
    let mapper = VocabPhonemeMap::for_language(tokens).expect("tokens.txt");
    let _ = name; // used only in panic messages above
    Bundle { lex, wfst, mapper }
}

/// Which tier resolves `word`, and whether the result is speakable by
/// the model — the same primitives and the same decision order as the
/// runtime front-end (dictionary lookup, WFST fallback, then the
/// Phase 2 vocabulary ladder on every phone).
fn classify(b: &mut Bundle, word: &str) -> &'static str {
    let key = word.to_lowercase();
    if b.lex.lookup(&key).is_some() {
        return "dict";
    }
    let Some(wfst) = b.wfst.as_mut() else {
        return "drop";
    };
    let phones = wfst.fallback(&key);
    if phones.is_empty() {
        return "drop";
    }
    if phones.iter().all(|p| b.mapper.map_symbol(p).is_some()) {
        "wfst"
    } else {
        "unmap"
    }
}

#[allow(clippy::cast_precision_loss)]
fn report(label: &str, t: &[Tally; 2]) {
    for (name, row) in [("de", &t[0]), ("deu", &t[1])] {
        let total = row.total().max(1);
        println!(
            "{label:<20} {name}: dict {:>6.2}%  wfst {:>6.2}%  unmap {:>6.3}%  drop {:>6.3}%",
            row.dict as f64 / total as f64 * 100.0,
            row.wfst as f64 / total as f64 * 100.0,
            row.unmap as f64 / total as f64 * 100.0,
            row.dropped as f64 / total as f64 * 100.0,
        );
    }
}

#[allow(clippy::cast_precision_loss)]
fn verdict(name: &str, wfst_exact: f64, t: &Tally) {
    let total = t.total().max(1);
    let share = (t.wfst + t.unmap) as f64 / total as f64 * 100.0;
    let expected_err = share / 100.0 * (1.0 - wfst_exact) * 100.0;
    println!(
        "{name}: WFST share {share:.2}% × holdout error {:.1}% → expected \
         mispronunciation ≈ {expected_err:.2}% of tokens",
        (1.0 - wfst_exact) * 100.0
    );
}
