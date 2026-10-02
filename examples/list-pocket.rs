#[cfg(feature = "sherpaonnx")]
fn main() {
    for (id, m) in sherpa_onnx_models::models() {
        if m.model_type == "pocket" {
            println!("{id} | {} | {}", m.name, m.url);
        }
    }
}

#[cfg(not(feature = "sherpaonnx"))]
fn main() {
    eprintln!("this example requires --features sherpaonnx");
}
