// Benchmark floravox vs sherpa-onnx on the same voice + text.
// Usage: cargo run --features sherpaonnx,floravox --example engine-bench -- --model-dir DIR --model NAME

// Demo examples are illustrative, not production code — relax pedantic lints.
#![allow(clippy::all, clippy::pedantic)]

use rust_tts_wrapper::engine::TtsEngine;
use std::time::Instant;

fn rss_kb() -> i64 {
    #[cfg(target_os = "macos")]
    {
        // No /proc on macOS; sample current RSS via ps (KB). Peak-vs-
        // current: Linux reads VmHWM (true peak); this is a point-in-time
        // sample, taken after synthesis when the arena is at its high
        // water mark, so it tracks the Linux number closely in practice.
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output();
        out.ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        for line in status.lines() {
            if let Some(v) = line.strip_prefix("VmHWM:") {
                return v.trim().trim_end_matches("kB").trim().parse().unwrap_or(0);
            }
        }
        0
    }
}

fn bench(engine: &dyn TtsEngine, name: &str, text: &str, runs: usize) {
    // warmup (model load happens here)
    let t0 = Instant::now();
    let mut n = engine
        .synth_to_bytes(text, None, 1.0, 1.0, 1.0)
        .map_or(0, |b| b.len());
    let cold = t0.elapsed();
    let mut best = std::time::Duration::MAX;
    for _ in 0..runs {
        let t = Instant::now();
        if let Ok(b) = engine.synth_to_bytes(text, None, 1.0, 1.0, 1.0) {
            n = b.len();
        }
        best = best.min(t.elapsed());
    }
    println!(
        "{name:12} cold(first synth) {:6.0} ms | warm best {:6.0} ms | audio {} B | peak RSS {:.1} MB",
        cold.as_millis(),
        best.as_millis(),
        n,
        rss_kb() as f64 / 1024.0,
    );
}

fn main() {
    // one engine per process when asked (clean RSS attribution)
    let only = std::env::var("BENCH_ONLY").ok();
    let mut dir = None;
    let mut model = String::new();
    let mut text = "Die Grenzen meiner Sprache sind die Grenzen meiner Welt. Der Zauberberg erstreckt sich über viele Seiten.".to_string();
    let mut runs = 3;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--model-dir" => dir = args.next(),
            "--model" => model = args.next().unwrap_or_default(),
            "--text" => text = args.next().unwrap_or(text),
            "--runs" => runs = args.next().and_then(|v| v.parse().ok()).unwrap_or(3),
            _ => {}
        }
    }
    let Some(dir) = dir else {
        eprintln!("--model-dir required");
        std::process::exit(2);
    };
    let base = rss_kb();
    println!(
        "baseline RSS before engines: {:.1} MB",
        base as f64 / 1024.0
    );
    println!("text: {text}\n");

    let creds = format!("{{\"modelsDir\":\"{}\",\"modelId\":\"{}\"}}", dir, model);
    if only.as_deref() != Some("sherpa") {
        let r0 = rss_kb();
        let floravox = rust_tts_wrapper::floravox_engine::FloravoxEngine::new(&creds);
        let r1 = rss_kb();
        let _ = floravox.get_voices();
        let r2 = rss_kb();
        bench(&floravox, "floravox", &text, runs);
        eprintln!(
            "floravox RSS: new +{:.0} MB, voices +{:.0} MB, total peak {:.0} MB",
            (r1 - r0) as f64 / 1024.0,
            (r2 - r1) as f64 / 1024.0,
            rss_kb() as f64 / 1024.0
        );
    }

    let sherpa_creds = format!("{{\"modelsDir\":\"{}\",\"modelId\":\"{}\"}}", dir, model);
    if only.as_deref() == Some("sherpa") || only.is_none() {
        if let Some(sherpa) = rust_tts_wrapper::create_engine("sherpaonnx", &sherpa_creds) {
            bench(sherpa.as_ref(), "sherpa", &text, runs);
        } else {
            println!("sherpa engine unavailable");
        }
    }
}
