//! Stage-split timing harness for the color-cluster spline pipeline.
//!
//!   cargo run --release --example timing -- [--runs N] <image> [<image> ...]
//!
//! For each image it reports wall-clock time for the pipeline stages that
//! matter to a parallelization plan:
//!
//!   * Stage 1 (CCL)        — connected-component labeling
//!   * Stage 2 (merge)      — hierarchical region merging
//!     (both measured by driving visioncortex's incremental builder directly
//!     and splitting on its progress() 50% boundary)
//!   * Segment total        — pipeline.segment(): stage 1+2 + keying + mask build
//!   * Finish               — pipeline.finish(): color-fit + compose(trace+spline) + optimize
//!   * SVG write            — serialization
//!
//! Times are the median of N runs (default 3) after one warm-up run.

use std::time::{Duration, Instant};

use vtracer::config::Config;
use vtracer::ColorImage;
use visioncortex::color_clusters::{Runner, RunnerConfig, KeyingAction, HIERARCHICAL_MAX};
use visioncortex::Color;

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

/// Time Stage 1 vs Stage 2 by driving the incremental builder and attributing
/// each tick to the stage reported *before* that tick runs. Mirrors the
/// RunnerConfig that ColorClusterFrontend::prepare builds from a default
/// Config (color_precision 6 -> loss 2, filter_speckle 4 -> good_min_area 16,
/// layer_difference 16), minus transparency keying.
fn stage_split(img: &ColorImage, cfg: &Config) -> (Duration, Duration) {
    let config = RunnerConfig {
        diagonal: cfg.layer_difference == 0,
        hierarchical: HIERARCHICAL_MAX,
        batch_size: 25600,
        good_min_area: cfg.filter_speckle * cfg.filter_speckle,
        good_max_area: img.width * img.height,
        is_same_color_a: 8 - cfg.color_precision,
        is_same_color_b: 1,
        deepen_diff: cfg.layer_difference,
        hollow_neighbours: 1,
        key_color: Color::default(),
        keying_action: KeyingAction::Discard,
    };
    let mut builder = Runner::new(config, img.clone()).start();
    let mut s1 = Duration::ZERO;
    let mut s2 = Duration::ZERO;
    loop {
        let in_stage1 = builder.progress() < 50;
        let t = Instant::now();
        let done = builder.tick();
        let dt = t.elapsed();
        if in_stage1 { s1 += dt } else { s2 += dt }
        if done { break }
    }
    (s1, s2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut runs = 3usize;
    let mut paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--runs" => { i += 1; runs = args[i].parse().expect("--runs N"); }
            p => paths.push(p.to_string()),
        }
        i += 1;
    }
    if paths.is_empty() {
        eprintln!("usage: timing [--runs N] <image> [<image> ...]");
        std::process::exit(2);
    }

    let cfg = Config::default(); // color-cluster, stacked, spline — the classic path
    let pipeline = cfg.build().expect("build pipeline");

    #[cfg(feature = "parallel")]
    println!("[compositing: PARALLEL (rayon)]");
    #[cfg(not(feature = "parallel"))]
    println!("[compositing: SEQUENTIAL]");

    println!(
        "{:<28} {:>7} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "image", "MP", "stage1", "stage2", "segment", "finish", "svg", "total"
    );
    println!("{}", "-".repeat(115));

    for path in &paths {
        let img = load(path);
        let mp = img.width * img.height;

        // Warm-up (allocator, caches) — result discarded.
        let _ = pipeline.segment(&img).and_then(|s| pipeline.finish(&s));

        let mut seg_v = Vec::new();
        let mut fin_v = Vec::new();
        let mut svg_v = Vec::new();
        let mut s1_v = Vec::new();
        let mut s2_v = Vec::new();

        for _ in 0..runs {
            let t = Instant::now();
            let seg = pipeline.segment(&img).expect("segment");
            seg_v.push(t.elapsed());

            let t = Instant::now();
            let doc = pipeline.finish(&seg).expect("finish");
            fin_v.push(t.elapsed());

            let t = Instant::now();
            let _svg = pipeline.writer.write(&doc);
            svg_v.push(t.elapsed());

            let (s1, s2) = stage_split(&img, &cfg);
            s1_v.push(s1);
            s2_v.push(s2);
        }

        let seg = median(seg_v);
        let fin = median(fin_v);
        let svg = median(svg_v);
        let s1 = median(s1_v);
        let s2 = median(s2_v);
        let total = seg + fin + svg;

        let name = std::path::Path::new(path)
            .file_name().unwrap().to_string_lossy();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        println!(
            "{:<28} {:>6.2}M {:>9.1}ms {:>9.1}ms {:>9.1}ms {:>9.1}ms {:>9.1}ms {:>9.1}ms",
            &name[..name.len().min(28)],
            mp as f64 / 1e6,
            ms(s1), ms(s2), ms(seg), ms(fin), ms(svg), ms(total),
        );
        // Percentage breakdown against total, plus the clustering-internal split.
        let pct = |d: Duration| d.as_secs_f64() / total.as_secs_f64() * 100.0;
        let clus = (s1 + s2).as_secs_f64();
        let s1p = if clus > 0.0 { s1.as_secs_f64() / clus * 100.0 } else { 0.0 };
        println!(
            "{:<28} {:>7} {:>9.0}% {:>9.0}% {:>9.0}% {:>9.0}% {:>9.0}%   (stage1 {:.0}% / stage2 {:.0}% of clustering)",
            "  % of total", "", pct(s1), pct(s2), pct(seg), pct(fin), pct(svg), s1p, 100.0 - s1p,
        );
    }
}
