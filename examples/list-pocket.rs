fn main() {
    for (id, m) in sherpa_onnx_models::models().iter() {
        if m.model_type == "pocket" {
            println!("{id} | {} | {}", m.name, m.url);
        }
    }
}
