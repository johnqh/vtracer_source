//! Isolated SVG-serialization microbenchmark.
//!
//!   cargo run --release --example svg_bench -- [--iters N] <image> [<image> ...]
//!
//! Segments + composes each image once (expensive, done once), then times only
//! `SvgWriter::write` over N iterations and reports the median ns/call and the
//! implied MB/s. Isolating the writer this way is far more robust to background
//! machine load than timing the whole pipeline.

use std::time::{Duration, Instant};

use vtracer::config::Config;
use vtracer::ColorImage;

fn load(path: &str) -> ColorImage {
    let img = image::open(path)
        .unwrap_or_else(|e| panic!("open {path}: {e}"))
        .to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    ColorImage { pixels: img.into_raw(), width: w, height: h }
}

fn median(mut xs: Vec<Duration>) -> Duration {
    xs.sort();
    xs[xs.len() / 2]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut iters = 50usize;
    let mut paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--iters" => { i += 1; iters = args[i].parse().expect("--iters N"); }
            p => paths.push(p.to_string()),
        }
        i += 1;
    }
    if paths.is_empty() {
        eprintln!("usage: svg_bench [--iters N] <image> [<image> ...]");
        std::process::exit(2);
    }

    let cfg = Config::default();
    let pipeline = cfg.build().expect("build pipeline");

    println!("{:<28} {:>10} {:>12} {:>10}", "image", "svg bytes", "median/call", "MB/s");
    println!("{}", "-".repeat(64));

    for path in &paths {
        let img = load(path);
        // Build the document once; only the writer is measured.
        let seg = pipeline.segment(&img).expect("segment");
        let doc = pipeline.finish(&seg).expect("finish");

        let bytes = pipeline.writer.write(&doc).len(); // warm-up + size

        let mut times = Vec::with_capacity(iters);
        for _ in 0..iters {
            let t = Instant::now();
            let s = pipeline.writer.write(&doc);
            times.push(t.elapsed());
            std::hint::black_box(&s);
        }
        let m = median(times);
        let mbps = bytes as f64 / m.as_secs_f64() / 1e6;

        let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy();
        println!(
            "{:<28} {:>10} {:>10.2}ms {:>10.0}",
            &name[..name.len().min(28)],
            bytes,
            m.as_secs_f64() * 1000.0,
            mbps,
        );
    }
}
