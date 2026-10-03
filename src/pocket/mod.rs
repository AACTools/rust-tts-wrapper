//! Pocket-TTS with real word timings: our own streaming inference over the
//! Kyutai ONNX graphs (lm_main + attention tap / lm_flow / mimi encoder +
//! decoder / text conditioner), so every generation step's attention over
//! the text tokens is captured — the model's own alignment, no aligner.
//!
//! Layout + recipe: ~/models/pocket-onnx/SPRINTS.md (validated in Python by
//! run_tapped.py; whisper cross-validation within ~0.17s on matched words).

pub mod model;
pub mod timings;
pub mod tokenizer;

pub use model::PocketTtsModel;
pub use timings::word_boundaries;
pub use tokenizer::PocketTokenizer;
