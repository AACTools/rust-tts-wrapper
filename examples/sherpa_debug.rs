// Direct sherpa-onnx debug harness: piper vits + our lexicon, debug on,
// to see OOV/token-drop logs from the lexicon frontend.
fn main() {
    let dir = "/home/willwade/.rust-tts-wrapper/sherpaonnx/piper-en_US-amy-low";
    let config = sherpa_onnx::OfflineTtsConfig {
        model: sherpa_onnx::OfflineTtsModelConfig {
            vits: sherpa_onnx::OfflineTtsVitsModelConfig {
                model: Some(format!("{dir}/en_US-amy-low.onnx")),
                tokens: Some(format!("{dir}/tokens.txt")),
                lexicon: Some(format!("{dir}/lexicon.txt")),
                ..Default::default()
            },
            num_threads: 2,
            debug: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let tts = sherpa_onnx::OfflineTts::create(&config).expect("create");
    let text = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "hello world".into());
    let audio = tts
        .generate_with_config(
            &text,
            &sherpa_onnx::GenerationConfig {
                sid: 0,
                speed: 1.0,
                ..Default::default()
            },
            Some(|_s: &[f32], _p: f32| -> bool { true }),
        )
        .expect("generate");
    println!(
        "generated {} samples @ {} Hz",
        audio.samples().len(),
        audio.sample_rate()
    );
}
