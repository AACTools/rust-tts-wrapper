// standalone: convert gruut lexicon to misaki + espeak conventions
// and generate lexicon.txt files for real models
//
// usage:
//   lexicon-convert <gruut.txt> <tokens.txt> <misaki|espeak> <out.txt> [pairs.tsv]
//
// pairs.tsv is optional; when given, the P2P table handles in-table
// words exactly and the rules cover the rest.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!(
            "usage: {} <gruut.txt> <tokens.txt> <misaki|espeak> <out.txt> [pairs.tsv]",
            args.first().map_or("lexicon-convert", String::as_str)
        );
        std::process::exit(2);
    }
    let gruut_path = &args[1];
    let tokens_path = &args[2];
    let convention = match args[3].as_str() {
        "misaki" => rust_tts_wrapper::sherpa_convention::Convention::Misaki,
        "espeak" => rust_tts_wrapper::sherpa_convention::Convention::Espeak,
        other => {
            eprintln!("convention must be 'misaki' or 'espeak', got {other:?}");
            std::process::exit(2);
        }
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
        Ok(stats) => println!(
            "written={} (table={}, rules={}) skipped={}",
            stats.written,
            stats.via_table,
            stats.rules(),
            stats.skipped
        ),
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}
