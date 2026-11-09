# CLAUDE.md

## What this is

`johnqh/vtracer_source` — a **performance fork** of `visioncortex/vtracer`
(VTracer, raster→SVG tracer), at version `1.0.0-alpha.3`. It is an **actively
maintained in-house fork**, not a vendored/read-only upstream copy — editing
source here is expected. Rust workspace with Rust/CLI/Python/Node surfaces.

## Three related repos (all `johnqh` forks)

- **this repo** — the VTracer framework (`crates/vtracer`), the `vtracer` CLI
  (`crates/vtracer-cli`), and the Python/Node bindings.
- **`../visioncortex`** (`johnqh/visioncortex`) — the core computer-vision lib
  it depends on; holds the clustering-stage optimizations. **Required sibling:**
  `Cargo.toml` has `[patch.crates-io] visioncortex = { path = "../visioncortex" }`,
  so the two MUST be checked out side by side or the build won't resolve
  (locally, in CI, and in Docker).
- **`../vtracer`** (`johnqh/vtracer`) — NOT upstream vtracer; it is the Docker
  deploy repo. Its `Dockerfile` clones both forks (pinned to exact commit SHAs)
  and builds `-p vtracer-cli`, publishing `johnqh/vtracer:latest`. `svgr_api`
  copies the binary out of that image — so that image is how this fork reaches
  production.

## Stack & layout

- Rust 2024 edition, `resolver = "2"`. Workspace members: `crates/vtracer`,
  `crates/vtracer-cli`, `crates/vtracer-bench`.
- **Excluded from the core workspace** (build from their own dirs, not `cargo
  --workspace`): `crates/vtracer-py` (pyo3 + maturin), `nodejs` (wasm-bindgen +
  wasm-pack), `webapp` (legacy pre-1.0 web app, kept in-tree but no longer
  built — don't wire it into anything).
- Core library pipeline (`crates/vtracer/src`): `frontend/` (segmentation:
  color_cluster, bw/binary, watershed) → `colorfit/` (oklab/palette/quantize/
  merge) → `compose.rs` + `mosaic/` (compositing, seam-free cutout) →
  `fitter.rs` + `simplify.rs` (curve fitting / simplification) → `optimize.rs`
  → `svg.rs`. IRs: `ir/` (`Segmentation`, `VectorDoc`). Entry types re-exported
  from `lib.rs`: `Config`, `Session`, `Pipeline`, `Preset`, `FitMode`,
  `Hierarchical`, `Clustering`. `Pipeline::segment()` caches; `finish()` re-runs.

## Verified commands

```sh
cargo build --workspace
cargo test  --workspace                 # golden/equivalence/pipeline tests in crates/vtracer/tests
cargo build --release -p vtracer-cli    # produces the `vtracer` binary
cargo build --target wasm32-unknown-unknown -p vtracer   # wasm-safety check
cd nodejs && wasm-pack build --target nodejs --out-dir pkg && node test.js
cd crates/vtracer-py && maturin develop  # maturin NOT installed by default here
```

`cargo`, `npm`, `wasm-pack` are on PATH; `maturin` is not (install it for the
Python build). The library's `parallel` feature (rayon) is on by default for
native; it's off the hot path for wasm (no threads) — keep the core wasm-safe
(dev-deps `resvg`/`image` are test/example-only, not compiled for wasm).

## Gotchas / conventions

- **CLI 0.6.x compat aliases (load-bearing).** `svgr_api` calls the binary with
  legacy vtracer 0.6.x flags: `--colormode` and snake_case `--filter_speckle`,
  `--color_precision`, `--gradient_step`, `--path_precision`,
  `--corner_threshold`, `--segment_length`, `--splice_threshold`. This CLI is
  kebab-case and renamed `--colormode`→`--clustering`, so
  `crates/vtracer-cli/src/main.rs` carries a clap `alias` for each. **Do not
  remove any alias** without updating `svgr_api/src/services/vtracer.ts` in
  lockstep. Some are `hide = true` (hidden from `--help`) but still accepted.
- **Deploy flow:** push here → in `johnqh/vtracer` repo bump the pinned
  `VTRACER_SOURCE_REF` (and `VISIONCORTEX_REF` if that changed) to the new
  commit SHA(s) AND bump `LABEL version` in its `Dockerfile` (CI only
  builds+pushes when the version isn't already on Docker Hub).
- **Branch naming is inconsistent — watch out.** The checked-out branch is
  `main` and `rust.yml` CI triggers on `main`/`0.*.x`, but `scripts/publish.sh`
  hard-requires being on `master` (`git push origin master`). `origin/main` and
  `origin/master` both exist. Confirm which branch before releasing.
- `scripts/publish.sh` pushes all surfaces (crates.io vtracer→vtracer-cli→
  vtracer-bench, npm, git tag→PyPI wheels, GitHub release→binaries); idempotent,
  needs `cargo/npm/gh` login; can't run in a sandbox. `--dry` for checks only.
- Version is set once in workspace `Cargo.toml` (`version.workspace = true`);
  npm/py versions must be bumped to match (npm `1.0.0-alpha.3`, PyPI `1.0.0a3`).
- README is upstream-flavored (crates.io/PyPI/npm links point at `visioncortex`
  packages); the fork-specific truth lives in its "This fork" section. Keep
  CLAUDE.md and that section consistent.
