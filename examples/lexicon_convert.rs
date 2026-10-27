// standalone: convert gruut lexicon to misaki + espeak conventions
// and generate lexicon.txt files for real models
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let gruut_path = &args[1];
    let tokens_path = &args[2];
    let out_path = &args[3];
    let convention = match args[4].as_str() {
        "misaki" => rust_tts_wrapper::sherpa_convention::Convention::Misaki,
        "espeak" => rust_tts_wrapper::sherpa_convention::Convention::Espeak,
        _ => panic!("convention must be 'misaki' or 'espeak'"),
    };
    match rust_tts_wrapper::sherpa_convention::convert_lexicon_file(
        std::path::Path::new(gruut_path),
        std::path::Path::new(tokens_path),
        convention,
        std::path::Path::new(out_path),
    ) {
        Ok((written, skipped)) => println!("written={written} skipped={skipped}"),
        Err(e) => println!("ERROR: {e}"),
    }
}
