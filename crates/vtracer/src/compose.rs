//! Compositing: turn a [`Segmentation`] into a [`VectorDoc`].
//!
//! * **Stacked** — each layer is traced independently into closed outlines and
//!   stacked in paint order (painter's algorithm).
//! * **Mosaic** — a seam-free gapless tessellation with shared boundary
//!   geometry (see [`crate::mosaic`]).
//!
//! Both compositors run the pipeline's [`CurvePass`]es over every fitted
//! contour before assembling paths — geometry passes have to happen here, on
//! the fitted geometry, so that in mosaic mode each shared boundary segment
//! is transformed exactly once for both of its faces.

use crate::error::Error;
use crate::fitter::CurveFitter;
use crate::ir::{MultiPath, RegionMask, Segmentation, Shape, VectorDoc};
use crate::mosaic::{compose_mosaic, SegmentFitter};
use crate::progress::{Ctx, Phase};
use crate::simplify::CurvePass;

#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// Which compositing strategy the pipeline uses. Each variant owns its fitter.
pub enum Compositing {
    /// Independent per-region closed outlines, stacked bottom-to-top.
    Stacked(Box<dyn CurveFitter>),
    /// Seam-free gapless tessellation via a shared boundary graph.
    Mosaic {
        fitter: Box<dyn SegmentFitter>,
        /// Merge flattened neighbours whose colors are within this diff —
        /// rejoins regions the stacked gradient layering had split. Usually
        /// the clustering gradient step; `0` still merges identical-color
        /// neighbours, negative disables merging entirely.
        merge_diff: i32,
    },
}

impl Compositing {
    /// Run the selected compositor over a segmentation, applying `passes` to
    /// every fitted contour before paths are assembled.
    pub fn compose(&self, seg: &Segmentation, passes: &[Box<dyn CurvePass>]) -> VectorDoc {
        match self {
            Compositing::Stacked(fitter) => compose_stacked(seg, fitter.as_ref(), passes),
            Compositing::Mosaic { fitter, merge_diff } => {
                compose_mosaic(seg, fitter.as_ref(), *merge_diff, passes)
            }
        }
    }

    /// Progress- and cancellation-aware compositing.
    ///
    /// Stacked mode reports per-layer progress and can be cancelled between
    /// layers. Mosaic builds its boundary graph in one pass, so it reports
    /// coarsely (start/end) and is cancellable only at the boundaries — the
    /// dominant cost is upstream in clustering, which cancels finely.
    pub fn compose_with(
        &self,
        seg: &Segmentation,
        passes: &[Box<dyn CurvePass>],
        ctx: &mut Ctx,
    ) -> Result<VectorDoc, Error> {
        match self {
            Compositing::Stacked(fitter) => compose_stacked_with(seg, fitter.as_ref(), passes, ctx),
            Compositing::Mosaic { fitter, merge_diff } => {
                ctx.check()?;
                ctx.report(Phase::Compose, 0.0);
                let doc = compose_mosaic(seg, fitter.as_ref(), *merge_diff, passes);
                ctx.check()?;
                ctx.report(Phase::Compose, 1.0);
                Ok(doc)
            }
        }
    }
}

/// Fit one region's outlines and run the curve passes over each contour.
/// Stacked contours are closed rings, so the ring form of each pass applies.
fn fit_region(
    fitter: &dyn CurveFitter,
    mask: &RegionMask,
    passes: &[Box<dyn CurvePass>],
) -> MultiPath {
    let mut path = MultiPath::new();
    for mut geom in fitter.fit_region(mask) {
        for pass in passes {
            geom = pass.ring(geom);
        }
        path.push(geom.into_closed_subpath());
    }
    path
}

/// Fit one layer into its painted [`Shape`], or `None` if it traced empty.
/// Pure over its inputs (the fitter and passes are `&`-shared), so it is safe
/// to call concurrently across layers.
fn fit_layer(
    fitter: &dyn CurveFitter,
    layer: &crate::ir::Layer,
    passes: &[Box<dyn CurvePass>],
) -> Option<Shape> {
    let path = fit_region(fitter, &layer.mask, passes);
    if path.is_empty() {
        None
    } else {
        Some(Shape {
            paint: layer.paint,
            path,
        })
    }
}

/// Progress-aware [`compose_stacked`]: reports after each layer and checks for
/// cancellation between them.
fn compose_stacked_with(
    seg: &Segmentation,
    fitter: &dyn CurveFitter,
    passes: &[Box<dyn CurvePass>],
    ctx: &mut Ctx,
) -> Result<VectorDoc, Error> {
    ctx.check()?;
    ctx.report(Phase::Compose, 0.0);

    // Parallel fits can't call the `&mut` progress sink, so Compose reports
    // coarsely (0 -> 1) like mosaic mode. Cancellation stays responsive: each
    // layer checks a cloned token and bails to `None`, and the post-region
    // `check()` turns a tripped token into `Error::Cancelled` before the
    // partial doc is used.
    #[cfg(feature = "parallel")]
    let shapes: Vec<Option<Shape>> = {
        let token = ctx.cancel_token();
        seg.layers
            .par_iter()
            .map(|layer| {
                if token.is_cancelled() {
                    return None;
                }
                fit_layer(fitter, layer, passes)
            })
            .collect()
    };

    // Sequential fallback keeps the original fine-grained per-layer progress.
    #[cfg(not(feature = "parallel"))]
    let shapes: Vec<Option<Shape>> = {
        let total = seg.layers.len().max(1);
        let mut out = Vec::with_capacity(seg.layers.len());
        for (i, layer) in seg.layers.iter().enumerate() {
            ctx.check()?;
            out.push(fit_layer(fitter, layer, passes));
            ctx.report(Phase::Compose, (i + 1) as f32 / total as f32);
        }
        out
    };

    ctx.check()?;

    let mut doc = VectorDoc::new(seg.width, seg.height);
    doc.shapes.extend(shapes.into_iter().flatten());
    ctx.report(Phase::Compose, 1.0);
    Ok(doc)
}

/// Trace every layer's closed outline and stack the shapes in paint order.
///
/// With the `parallel` feature the per-layer fits run concurrently; the
/// order-preserving `collect` keeps the bottom-to-top paint order identical to
/// the sequential path, so the output is unchanged.
pub fn compose_stacked(
    seg: &Segmentation,
    fitter: &dyn CurveFitter,
    passes: &[Box<dyn CurvePass>],
) -> VectorDoc {
    let mut doc = VectorDoc::new(seg.width, seg.height);

    #[cfg(feature = "parallel")]
    let shapes: Vec<Option<Shape>> = seg
        .layers
        .par_iter()
        .map(|layer| fit_layer(fitter, layer, passes))
        .collect();

    #[cfg(not(feature = "parallel"))]
    let shapes: Vec<Option<Shape>> = seg
        .layers
        .iter()
        .map(|layer| fit_layer(fitter, layer, passes))
        .collect();

    doc.shapes.extend(shapes.into_iter().flatten());
    doc
}
