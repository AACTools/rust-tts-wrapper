// standalone: convert gruut lexicon to misaki + espeak conventions
// and generate lexicon.txt files for real models
//
// usage:
//   lexicon_convert <gruut.txt> <tokens.txt> <misaki|espeak> <out.txt> [pairs.tsv]
//
// pairs.tsv is optional; when given, the P2P table handles in-table
// words exactly and the rules cover the rest.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let gruut_path = &args[1];
    let tokens_path = &args[2];
    let convention = match args[3].as_str() {
        "misaki" => rust_tts_wrapper::sherpa_convention::Convention::Misaki,
        "espeak" => rust_tts_wrapper::sherpa_convention::Convention::Espeak,
        _ => panic!("convention must be 'misaki' or 'espeak'"),
    };
    let out_path = &args[4];
    let pairs = args.get(5).map(|p| {
        rust_tts_wrapper::sherpa_p2p::P2pTable::from_tsv(std::path::Path::new(p))
            .unwrap_or_else(|e| panic!("loading pairs {p}: {e}"))
    });

    match rust_tts_wrapper::sherpa_convention::convert_lexicon_file(
        std::path::Path::new(gruut_path),
        std::path::Path::new(tokens_path),
        convention,
        pairs.as_ref(),
        std::path::Path::new(out_path),
    ) {
        Ok((written, skipped, via_table)) => {
            let rules = written - via_table;
            println!("written={written} (table={via_table}, rules={rules}) skipped={skipped}");
        }
        Err(e) => println!("ERROR: {e}"),
    }
}
