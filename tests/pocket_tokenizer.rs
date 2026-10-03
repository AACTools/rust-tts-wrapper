//! The Rust tokenizer must produce sherpa's exact ids (their debug print:
//! "Where is the bathroom" -> [1641, 277, 265, 3237, 1840]).
#![cfg(feature = "pocket-timing")]
use rust_tts_wrapper::pocket::PocketTokenizer;

#[test]
fn matches_sherpa_reference_ids() {
    let dir = std::path::PathBuf::from("/home/willwade/models/pocket-onnx");
    if !dir.join("vocab.json").is_file() {
        eprintln!("skipping: bundle not present");
        return;
    }
    let t = PocketTokenizer::load(&dir.join("vocab.json"), &dir.join("token_scores.json")).unwrap();
    let ids = t.encode("Where is the bathroom");
    assert_eq!(
        ids,
        vec![1641, 277, 265, 3237, 1840],
        "tokens: {:?}",
        ids.iter().map(|i| t.id_to_piece(*i)).collect::<Vec<_>>()
    );
    let ids2 = t.encode("The quick brown fox jumps over the lazy dog");
    assert_eq!(
        ids2.len(),
        13,
        "pieces: {:?}",
        ids2.iter().map(|i| t.id_to_piece(*i)).collect::<Vec<_>>()
    );
}
