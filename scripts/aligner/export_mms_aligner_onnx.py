#!/usr/bin/env python3
"""Export the MMS multilingual uroman CTC aligner to ONNX (#109).

Replicates fairseq examples/mms/data_prep/align_utils.py load_model_dict():
the torchaudio wav2vec2 architecture + the downloaded state dict, wrapped
so the graph is (waveform [1, T] float32) -> (logits [1, T', 31]).

Usage: venv/bin/python export_onnx.py
"""
import torch
from torchaudio.models import wav2vec2_model

import os
DIR = os.environ.get("MMS_ALIGNER_DIR", os.path.expanduser("~/.cache/rust-tts-wrapper/mms-aligner"))


class Aligner(torch.nn.Module):
    """Logits-only wrapper: the reference calls model(waveform) and uses
    model_outs[0]; we export exactly that."""

    def __init__(self, m):
        super().__init__()
        self.m = m

    def forward(self, waveform: torch.Tensor) -> torch.Tensor:
        out, _ = self.m(waveform)
        return out


def main() -> None:
    state_dict = torch.load(f"{DIR}/model.pt", map_location="cpu")
    model = wav2vec2_model(
        extractor_mode="layer_norm",
        extractor_conv_layer_config=[
            (512, 10, 5),
            (512, 3, 2),
            (512, 3, 2),
            (512, 3, 2),
            (512, 3, 2),
            (512, 2, 2),
            (512, 2, 2),
        ],
        extractor_conv_bias=True,
        encoder_embed_dim=1024,
        encoder_projection_dropout=0.0,
        encoder_pos_conv_kernel=128,
        encoder_pos_conv_groups=16,
        encoder_num_layers=24,
        encoder_num_heads=16,
        encoder_attention_dropout=0.0,
        encoder_ff_interm_features=4096,
        encoder_ff_interm_dropout=0.1,
        encoder_dropout=0.0,
        encoder_layer_norm_first=True,
        encoder_layer_drop=0.1,
        aux_num_out=31,
    )
    model.load_state_dict(state_dict)
    model.eval()

    aligner = Aligner(model).eval()

    # 5 seconds of 16 kHz audio (deterministic input for the trace).
    example = torch.randn(1, 80_000)

    with torch.inference_mode():
        torch.onnx.export(
            aligner,
            (example,),
            f"{DIR}/mms-aligner.onnx",
            input_names=["waveform"],
            output_names=["logits"],
            dynamic_axes={
                "waveform": {1: "samples"},
                "logits": {1: "frames"},
            },
            opset_version=17,
            do_constant_folding=True,
        )
    print("exported", f"{DIR}/mms-aligner.onnx")

    # Round-trip check: torch vs onnxruntime on a fresh input.
    import onnxruntime as ort

    wave = torch.randn(1, 63_123) * 0.05
    with torch.inference_mode():
        ref = aligner(wave).numpy()
    sess = ort.InferenceSession(
        f"{DIR}/mms-aligner.onnx", providers=["CPUExecutionProvider"]
    )
    got = sess.run(None, {"waveform": wave.numpy()})[0]
    import numpy as np

    print("shapes:", ref.shape, got.shape)
    print("max abs diff:", float(np.abs(ref - got).max()))
    assert np.abs(ref - got).max() < 1e-3, "ONNX divergence"
    print("OK")


if __name__ == "__main__":
    main()
