#!/usr/bin/env python3
"""Reference alignment oracle for the #109 Rust port cross-check.

Runs the exact fairseq examples/mms/data_prep pipeline on a test wav
(torch model -> emissions -> torchaudio forced_align -> get_spans),
prints the word timings, and dumps the emissions + token sequence for
the Rust side to align against (isolating Viterbi correctness from
model numerics).
"""
import json
import math
import re
import sys

import torch
import torchaudio
import torchaudio.functional as F
from torchaudio.models import wav2vec2_model

import os
DIR = os.environ.get("MMS_ALIGNER_DIR", os.path.expanduser("~/.cache/rust-tts-wrapper/mms-aligner"))
WAV = sys.argv[1] if len(sys.argv) > 1 else f"{DIR}/test16k.wav"
TEXT = sys.argv[2] if len(sys.argv) > 2 else "Hello, this is the control probe."

SAMPLING_FREQ = 16000


def normalize_uroman(text):
    text = text.lower()
    text = re.sub("([^a-z' ])", " ", text)
    text = re.sub(" +", " ", text)
    return text.strip()


def load_model_dict():
    state_dict = torch.load(f"{DIR}/model.pt", map_location="cpu")
    model = wav2vec2_model(
        extractor_mode="layer_norm",
        extractor_conv_layer_config=[
            (512, 10, 5), (512, 3, 2), (512, 3, 2),
            (512, 3, 2), (512, 3, 2), (512, 2, 2), (512, 2, 2),
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
    dictionary = {}
    with open(f"{DIR}/dictionary.txt") as f:
        dictionary = {l.strip(): i for i, l in enumerate(f.readlines())}
    return model, dictionary


def main():
    model, dictionary = load_model_dict()
    import soundfile as _sf
    data, sr = _sf.read(WAV, dtype="float32")
    waveform = torch.from_numpy(data).unsqueeze(0)
    assert sr == SAMPLING_FREQ, sr
    with torch.inference_mode():
        model_outs, _ = model(waveform)
        emissions = torch.log_softmax(model_outs[0], dim=-1)
    T, N = emissions.size()
    stride = float(waveform.size(1) * 1000 / T / SAMPLING_FREQ)
    print(f"emissions: T={T} N={N}, stride={stride:.4f} ms/frame")

    tokens = normalize_uroman(TEXT)
    token_indices = [dictionary[c] for c in " ".join(tokens).split(" ") if c in dictionary]
    blank = dictionary["<blank>"]
    targets = torch.tensor(token_indices, dtype=torch.int32)
    input_lengths = torch.tensor(emissions.shape[0]).unsqueeze(-1)
    target_lengths = torch.tensor(targets.shape[0]).unsqueeze(-1)
    path, _ = F.forced_align(
        emissions.unsqueeze(0), targets.unsqueeze(0), input_lengths, target_lengths, blank=blank
    )
    path = path.squeeze().tolist()

    # reference merge_repeats / get_spans (verbatim from align_utils)
    from dataclasses import dataclass

    @dataclass
    class Segment:
        label: str
        start: int
        end: int

    def merge_repeats(path, idx_to_token_map):
        i1, i2 = 0, 0
        segments = []
        while i1 < len(path):
            while i2 < len(path) and path[i1] == path[i2]:
                i2 += 1
            segments.append(Segment(idx_to_token_map[path[i1]], i1, i2 - 1))
            i1 = i2
        return segments

    segments = merge_repeats(path, {v: k for k, v in dictionary.items()})
    print("segments:", " ".join(f"{s.label}:{s.start}-{s.end}" for s in segments))

    words = tokens.split(" ")
    # word char segment indices (into `segments`)
    word_chars = []
    seg_cursor = 0
    token_chars = list(tokens.replace(" ", ""))
    for w in words:
        idxs = []
        for ch in w:
            # find the next segment with this label
            while seg_cursor < len(segments) and segments[seg_cursor].label != ch:
                seg_cursor += 1
            if seg_cursor < len(segments):
                idxs.append(seg_cursor)
                seg_cursor += 1
        word_chars.append(idxs)

    def get_spans(word_chars, segments, blank_label="<blank>"):
        spans = []
        words_with = [i for i, v in enumerate(word_chars) if v]
        if not words_with:
            return spans
        first_w, last_w = words_with[0], words_with[-1]
        for wi in words_with:
            k0, k1 = word_chars[wi][0], word_chars[wi][-1]
            start, end = segments[k0].start, segments[k1].end
            if k0 > 0 and segments[k0 - 1].label == blank_label:
                prev = segments[k0 - 1]
                pad = prev.start if wi == first_w else int((prev.start + prev.end) / 2)
                start = min(start, pad)
            if k1 + 1 < len(segments) and segments[k1 + 1].label == blank_label:
                nxt = segments[k1 + 1]
                pad = nxt.end if wi == last_w else math.floor((nxt.start + nxt.end) / 2)
                end = max(end, pad)
            spans.append((wi, start * stride / 1000, (end + 1) * stride / 1000))
        return spans

    spans = get_spans(word_chars, segments)
    print("\nORACLE word timings:")
    for wi, s, e in spans:
        print(f"  {words[wi]!r}: {s:.3f} .. {e:.3f}")

    # Dump for the Rust cross-check: emissions + token ids + spans.
    emissions.numpy().tofile(f"{DIR}/oracle_emissions.f32")
    with open(f"{DIR}/oracle.json", "w") as f:
        json.dump(
            {
                "text": TEXT,
                "tokens": tokens,
                "words": words,
                "token_indices": token_indices,
                "blank": blank,
                "char_ids": {c: i for c, i in dictionary.items() if len(c) == 1},
                "stride_ms": stride,
                "spans": spans,
            },
            f,
        )
    print(f"\nwrote {DIR}/oracle_emissions.f32 + oracle.json")


if __name__ == "__main__":
    main()
