//! Serialize a [`VectorDoc`] to an SVG string.
//!
//! The writer makes the encoding choices that shrink output without changing
//! geometry:
//!
//! * per segment, the shorter of absolute vs. relative deltas (`L`/`l`, `C`/`c`);
//! * `H`/`V` (`h`/`v`) for axis-aligned lines and `S`/`s` for smooth cubic
//!   continuations;
//! * compact number formatting (trimmed zeros, leading-dot decimals, no
//!   separator before a negative);
//! * optional `<g fill>` grouping of consecutive same-fill shapes.
//!
//! Coordinates are assumed to already be in absolute document space (the
//! [`crate::optimize::QuantizePass`] bakes in any offset), so no per-path
//! `transform` is emitted.

use std::fmt::Write as _;

use visioncortex::PointF64;

use crate::ir::{Paint, PathCmd, Shape, SubPath, VectorDoc};

/// SVG serializer configuration.
#[derive(Debug, Clone, Copy)]
pub struct SvgWriter {
    /// Allow relative commands where they serialize shorter.
    pub relative: bool,
    /// Allow `H`/`V`/`S` shorthands and `<g fill>` grouping.
    pub shorthands: bool,
    /// Decimal places for coordinates (`None` = full precision).
    pub precision: Option<u32>,
}

impl Default for SvgWriter {
    fn default() -> Self {
        Self {
            relative: true,
            shorthands: true,
            precision: Some(2),
        }
    }
}

impl SvgWriter {
    pub fn write(&self, doc: &VectorDoc) -> String {
        let mut out = String::new();
        out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        let _ = writeln!(
            out,
            "<!-- Generator: visioncortex VTracer {} -->",
            env!("CARGO_PKG_VERSION")
        );
        let _ = writeln!(
            out,
            "<svg version=\"1.1\" xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\">",
            doc.width, doc.height
        );

        if self.shorthands {
            self.write_grouped(&mut out, &doc.shapes);
        } else {
            for shape in &doc.shapes {
                self.write_path(&mut out, shape, true);
            }
        }

        out.push_str("</svg>\n");
        out
    }

    /// Emit shapes, grouping maximal runs of consecutive same-fill shapes into
    /// a single `<g fill>` (preserving paint order).
    fn write_grouped(&self, out: &mut String, shapes: &[Shape]) {
        let mut i = 0;
        while i < shapes.len() {
            let fill = shape_fill(&shapes[i]);
            let mut j = i + 1;
            while j < shapes.len() && shape_fill(&shapes[j]) == fill {
                j += 1;
            }
            let run = &shapes[i..j];
            if run.len() > 1 {
                let _ = writeln!(out, "<g fill=\"{}\">", fill);
                for shape in run {
                    self.write_path(out, shape, false);
                }
                out.push_str("</g>\n");
            } else {
                self.write_path(out, &run[0], true);
            }
            i = j;
        }
    }

    fn write_path(&self, out: &mut String, shape: &Shape, with_fill: bool) {
        let d = self.encode_path(shape);
        if d.is_empty() {
            return;
        }
        if with_fill {
            let _ = writeln!(
                out,
                "<path d=\"{}\" fill=\"{}\"/>",
                d,
                shape_fill(shape)
            );
        } else {
            let _ = writeln!(out, "<path d=\"{}\"/>", d);
        }
    }

    fn encode_path(&self, shape: &Shape) -> String {
        let mut emitter = Emitter::new(self.relative, self.shorthands, self.precision);
        for sub in &shape.path.subpaths {
            emitter.subpath(sub);
        }
        emitter.finish()
    }
}

fn shape_fill(shape: &Shape) -> String {
    match shape.paint {
        Paint::Solid(c) => c.to_hex_string(),
    }
}

/// Streaming SVG-path encoder that tracks the current point.
///
/// Numbers and candidate tokens are formatted into reused scratch buffers
/// (`cand`/`numbuf`) rather than fresh allocations per segment; the shortest
/// candidate is kept in `best`. Selection is identical to the old
/// `min_by_key(len)` / `shorter` logic (first candidate wins ties), so output
/// is byte-for-byte unchanged.
struct Emitter {
    relative: bool,
    shorthands: bool,
    precision: Option<u32>,
    out: String,
    cur: PointF64,
    /// Start of the current subpath; `cur` returns here after `Z`.
    subpath_start: PointF64,
    started: bool,
    /// Absolute second control point of the previous cubic, for `S` detection.
    prev_cubic_c2: Option<PointF64>,
    /// Scratch: the candidate token currently being built.
    cand: String,
    /// Scratch: the shortest candidate seen for the current command.
    best: String,
    /// Scratch: one number at a time (for trimming before it lands in `cand`).
    numbuf: String,
}

impl Emitter {
    fn new(relative: bool, shorthands: bool, precision: Option<u32>) -> Self {
        Self {
            relative,
            shorthands,
            precision,
            out: String::new(),
            cur: PointF64::default(),
            subpath_start: PointF64::default(),
            started: false,
            prev_cubic_c2: None,
            cand: String::new(),
            best: String::new(),
            numbuf: String::new(),
        }
    }

    fn finish(self) -> String {
        self.out
    }

    fn subpath(&mut self, sub: &SubPath) {
        for cmd in &sub.commands {
            match *cmd {
                PathCmd::MoveTo(p) => self.move_to(p),
                PathCmd::LineTo(p) => self.line_to(p),
                PathCmd::CubicTo(c1, c2, e) => self.cubic_to(c1, c2, e),
                PathCmd::Close => {
                    self.out.push('Z');
                    // SVG resets the current point to the subpath's start after
                    // Z; a following relative `m`/`l` is measured from there.
                    self.cur = self.subpath_start;
                    self.prev_cubic_c2 = None;
                }
            }
        }
    }

    fn move_to(&mut self, p: PointF64) {
        let Emitter { out, cand, best, numbuf, cur, precision, relative, started, subpath_start, prev_cubic_c2, .. } = self;
        if !*started {
            // First move is always absolute.
            out.push('M');
            push_coord(out, numbuf, p, *precision);
            *started = true;
        } else {
            best.clear();
            cand.clear();
            cand.push('M');
            push_coord(cand, numbuf, p, *precision);
            consider(best, cand);
            if *relative {
                cand.clear();
                cand.push('m');
                push_coord_delta(cand, numbuf, p, *cur, *precision);
                consider(best, cand);
            }
            out.push_str(best);
        }
        *cur = p;
        *subpath_start = p;
        *prev_cubic_c2 = None;
    }

    fn line_to(&mut self, p: PointF64) {
        let Emitter { out, cand, best, numbuf, cur, precision, relative, shorthands, prev_cubic_c2, .. } = self;
        best.clear();

        // Axis-aligned shorthands.
        if *shorthands {
            if p.y == cur.y {
                cand.clear();
                cand.push('H');
                push_list_num(cand, numbuf, p.x, *precision, true);
                consider(best, cand);
                if *relative {
                    cand.clear();
                    cand.push('h');
                    push_list_num(cand, numbuf, p.x - cur.x, *precision, true);
                    consider(best, cand);
                }
            }
            if p.x == cur.x {
                cand.clear();
                cand.push('V');
                push_list_num(cand, numbuf, p.y, *precision, true);
                consider(best, cand);
                if *relative {
                    cand.clear();
                    cand.push('v');
                    push_list_num(cand, numbuf, p.y - cur.y, *precision, true);
                    consider(best, cand);
                }
            }
        }

        cand.clear();
        cand.push('L');
        push_coord(cand, numbuf, p, *precision);
        consider(best, cand);
        if *relative {
            cand.clear();
            cand.push('l');
            push_coord_delta(cand, numbuf, p, *cur, *precision);
            consider(best, cand);
        }

        out.push_str(best);
        *cur = p;
        *prev_cubic_c2 = None;
    }

    fn cubic_to(&mut self, c1: PointF64, c2: PointF64, e: PointF64) {
        let Emitter { out, cand, best, numbuf, cur, precision, relative, shorthands, prev_cubic_c2, .. } = self;
        best.clear();

        // Smooth continuation: c1 is the reflection of the previous cubic's c2.
        if *shorthands {
            if let Some(prev_c2) = *prev_cubic_c2 {
                let reflection = PointF64 {
                    x: 2.0 * cur.x - prev_c2.x,
                    y: 2.0 * cur.y - prev_c2.y,
                };
                if approx(reflection, c1) {
                    cand.clear();
                    cand.push('S');
                    push_coord_list(cand, numbuf, &[c2, e], *precision);
                    consider(best, cand);
                    if *relative {
                        cand.clear();
                        cand.push('s');
                        push_delta_list(cand, numbuf, &[c2, e], *cur, *precision);
                        consider(best, cand);
                    }
                }
            }
        }

        cand.clear();
        cand.push('C');
        push_coord_list(cand, numbuf, &[c1, c2, e], *precision);
        consider(best, cand);
        if *relative {
            cand.clear();
            cand.push('c');
            push_delta_list(cand, numbuf, &[c1, c2, e], *cur, *precision);
            consider(best, cand);
        }

        out.push_str(best);
        *cur = e;
        *prev_cubic_c2 = Some(c2);
    }
}

fn approx(a: PointF64, b: PointF64) -> bool {
    (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6
}

/// Keep the shortest candidate; ties keep the earlier one (matches the old
/// `min_by_key`/`shorter`). `best` empty means no candidate chosen yet.
fn consider(best: &mut String, cand: &str) {
    if best.is_empty() || cand.len() < best.len() {
        best.clear();
        best.push_str(cand);
    }
}

/// Append one number to `buf`, prefixing the SVG list separator (a comma,
/// unless the number is self-separating with a leading `-`, or it is the first
/// in its list). Mirrors [`join_nums`] streamed one number at a time.
fn push_list_num(buf: &mut String, numbuf: &mut String, v: f64, precision: Option<u32>, first: bool) {
    let start = buf.len();
    push_num(buf, numbuf, v, precision);
    if !first && buf.as_bytes()[start] != b'-' {
        // The number sits at `start..`; splicing a comma in front of it shifts
        // only those few bytes.
        buf.insert(start, ',');
    }
}

/// Absolute coordinate pair after a command letter.
fn push_coord(buf: &mut String, numbuf: &mut String, p: PointF64, precision: Option<u32>) {
    push_list_num(buf, numbuf, p.x, precision, true);
    push_list_num(buf, numbuf, p.y, precision, false);
}

/// Delta coordinate pair relative to `cur`.
fn push_coord_delta(buf: &mut String, numbuf: &mut String, p: PointF64, cur: PointF64, precision: Option<u32>) {
    push_list_num(buf, numbuf, p.x - cur.x, precision, true);
    push_list_num(buf, numbuf, p.y - cur.y, precision, false);
}

/// Absolute list of points, flattened.
fn push_coord_list(buf: &mut String, numbuf: &mut String, pts: &[PointF64], precision: Option<u32>) {
    for (i, p) in pts.iter().enumerate() {
        push_list_num(buf, numbuf, p.x, precision, i == 0);
        push_list_num(buf, numbuf, p.y, precision, false);
    }
}

/// Delta list of points relative to `cur` (all deltas are from `cur`, matching
/// SVG's relative-command semantics for multi-point ops).
fn push_delta_list(buf: &mut String, numbuf: &mut String, pts: &[PointF64], cur: PointF64, precision: Option<u32>) {
    for (i, p) in pts.iter().enumerate() {
        push_list_num(buf, numbuf, p.x - cur.x, precision, i == 0);
        push_list_num(buf, numbuf, p.y - cur.y, precision, false);
    }
}

/// Append compact number formatting to `buf`: round to precision, trim trailing
/// zeros, leading-dot for magnitudes below 1. `numbuf` is reused scratch.
///
/// For fixed precision the number is formatted from the scaled *integer*
/// `round(v * 10^p)` — integer formatting, not float. This is byte-identical to
/// the old `format!("{:.p}", (v*f).round()/f)`: below ~2^51 the scaled value is
/// an exact f64 integer and `{:.p}` of it reproduces exactly those digits, so
/// the two agree. Values above that safe range (or `precision == None`) fall
/// back to the float formatter.
fn push_num(buf: &mut String, numbuf: &mut String, v: f64, precision: Option<u32>) {
    if let Some(p) = precision {
        if p <= 15 {
            let factor = 10f64.powi(p as i32);
            let scaled = (v * factor).round();
            // Normalize -0.0 to 0.
            if scaled == 0.0 {
                buf.push('0');
                return;
            }
            if scaled.abs() < 9.0e15 {
                push_num_fixed(buf, numbuf, scaled as i64, p);
                return;
            }
        }
    }
    push_num_float(buf, numbuf, v, precision);
}

/// Fixed-precision integer path: `n` is `round(v * 10^p)`, already non-zero.
fn push_num_fixed(buf: &mut String, numbuf: &mut String, n: i64, p: u32) {
    if n < 0 {
        buf.push('-');
    }
    let a = n.unsigned_abs();
    let scale = 10u64.pow(p);
    let int = a / scale;
    let frac = a % scale;
    if int != 0 {
        let _ = write!(buf, "{int}");
    }
    // int == 0 emits the leading-dot form (".5", "-.5") — no "0" before the dot.
    if frac != 0 {
        buf.push('.');
        numbuf.clear();
        let _ = write!(numbuf, "{frac:0width$}", width = p as usize);
        buf.push_str(numbuf.trim_end_matches('0'));
    }
}

/// Float-formatter fallback (full precision, or magnitudes past the safe
/// integer range). Same trim/leading-dot rules as the fixed path.
fn push_num_float(buf: &mut String, numbuf: &mut String, v: f64, precision: Option<u32>) {
    let v = match precision {
        Some(p) => {
            let factor = 10f64.powi(p as i32);
            (v * factor).round() / factor
        }
        None => v,
    };
    if v == 0.0 {
        buf.push('0');
        return;
    }

    numbuf.clear();
    match precision {
        Some(p) => {
            let _ = write!(numbuf, "{:.*}", p as usize, v);
        }
        None => {
            let _ = write!(numbuf, "{v}");
        }
    }

    let mut s: &str = numbuf;
    if s.contains('.') {
        s = s.trim_end_matches('0');
        s = s.trim_end_matches('.');
    }

    if let Some(rest) = s.strip_prefix("0.") {
        buf.push('.');
        buf.push_str(rest);
    } else if let Some(rest) = s.strip_prefix("-0.") {
        buf.push_str("-.");
        buf.push_str(rest);
    } else {
        buf.push_str(s);
    }
}

/// Join formatted numbers with the minimal separators SVG allows: a comma,
/// except that a leading `-` is self-separating. (Retained for tests; the
/// encoder streams via [`push_list_num`].)
#[cfg(test)]
fn join_nums(nums: &[String]) -> String {
    let mut s = String::new();
    for (i, n) in nums.iter().enumerate() {
        if i > 0 && !n.starts_with('-') {
            s.push(',');
        }
        s.push_str(n);
    }
    s
}

/// Compact number formatting (owned-`String` form; retained for tests).
#[cfg(test)]
fn fmt_num(v: f64, precision: Option<u32>) -> String {
    let mut buf = String::new();
    let mut numbuf = String::new();
    push_num(&mut buf, &mut numbuf, v, precision);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{MultiPath, Paint, PathCmd, Shape, SubPath};
    use visioncortex::Color;

    /// The integer fast path must be byte-identical to the float formatter it
    /// replaces, across a wide range of magnitudes, signs, and precisions.
    #[test]
    fn fixed_path_matches_float_path() {
        for &p in &[0u32, 1, 2, 3, 4] {
            let factor = 10f64.powi(p as i32);
            // A spread of values incl. carries, negatives, sub-1, round numbers.
            let mut v = -5000.0f64;
            while v <= 5000.0 {
                let mut fixed = String::new();
                let mut float = String::new();
                let mut scratch = String::new();
                push_num(&mut fixed, &mut scratch, v, Some(p));
                push_num_float(&mut float, &mut scratch, v, Some(p));
                assert_eq!(fixed, float, "mismatch at v={v}, p={p}");
                v += 1.0 / (factor / 7.0).max(1.0) + 0.017;
            }
        }
    }

    #[test]
    fn number_formatting() {
        assert_eq!(fmt_num(0.0, Some(2)), "0");
        assert_eq!(fmt_num(-0.0, Some(2)), "0");
        assert_eq!(fmt_num(1.50, Some(2)), "1.5");
        assert_eq!(fmt_num(0.5, Some(2)), ".5");
        assert_eq!(fmt_num(-0.5, Some(2)), "-.5");
        assert_eq!(fmt_num(2.0, Some(2)), "2");
        assert_eq!(fmt_num(3.14159, Some(2)), "3.14");
    }

    #[test]
    fn join_omits_separator_before_negative() {
        let nums = vec!["1".to_string(), "-2".to_string(), "3".to_string()];
        assert_eq!(join_nums(&nums), "1-2,3");
    }

    fn square_shape() -> Shape {
        use visioncortex::PointF64;
        let p = |x, y| PointF64 { x, y };
        let mut sub = SubPath::new();
        sub.commands = vec![
            PathCmd::MoveTo(p(0.0, 0.0)),
            PathCmd::LineTo(p(10.0, 0.0)),
            PathCmd::LineTo(p(10.0, 10.0)),
            PathCmd::LineTo(p(0.0, 10.0)),
            PathCmd::Close,
        ];
        Shape {
            paint: Paint::Solid(Color::new(255, 0, 0)),
            path: MultiPath { subpaths: vec![sub] },
        }
    }

    #[test]
    fn encodes_axis_aligned_shorthands() {
        let writer = SvgWriter {
            relative: true,
            shorthands: true,
            precision: Some(2),
        };
        let d = writer.encode_path(&square_shape());
        // Horizontal/vertical lines collapse to H/V/h/v; first move is absolute.
        assert!(d.starts_with("M0,0"));
        assert!(d.contains('H') || d.contains('h'));
        assert!(d.contains('V') || d.contains('v'));
        assert!(d.ends_with('Z'));
    }

    #[test]
    fn absolute_mode_uses_no_relative_commands() {
        let writer = SvgWriter {
            relative: false,
            shorthands: false,
            precision: Some(2),
        };
        let d = writer.encode_path(&square_shape());
        assert!(!d.contains('l'));
        assert!(!d.contains('c'));
        assert!(d.contains('L'));
    }

    /// A shape with a hole (second subpath). Encoded absolute vs relative must
    /// describe the *same* geometry — regression for the bug where the current
    /// point was not reset to the subpath start after `Z`, so the relative `m`
    /// of the hole was measured from the wrong origin.
    fn holed_shape() -> Shape {
        use visioncortex::PointF64;
        let p = |x, y| PointF64 { x, y };
        let outer = SubPath {
            commands: vec![
                PathCmd::MoveTo(p(0.0, 0.0)),
                PathCmd::LineTo(p(30.0, 0.0)),
                PathCmd::LineTo(p(30.0, 30.0)),
                PathCmd::LineTo(p(0.0, 30.0)),
                PathCmd::Close,
            ],
        };
        let hole = SubPath {
            commands: vec![
                PathCmd::MoveTo(p(10.0, 10.0)),
                PathCmd::LineTo(p(20.0, 10.0)),
                PathCmd::LineTo(p(20.0, 20.0)),
                PathCmd::LineTo(p(10.0, 20.0)),
                PathCmd::Close,
            ],
        };
        Shape {
            paint: Paint::Solid(Color::new(0, 0, 0)),
            path: MultiPath {
                subpaths: vec![outer, hole],
            },
        }
    }

    /// Parse an SVG `d` (M/m/L/l/H/h/V/v/Z only) into absolute points.
    fn parse_abs(d: &str) -> Vec<(f64, f64)> {
        let mut toks = Vec::new();
        let mut i = 0;
        let b = d.as_bytes();
        while i < b.len() {
            let c = b[i] as char;
            if c.is_ascii_alphabetic() {
                toks.push(c.to_string());
                i += 1;
            } else if c == '-' || c == '.' || c.is_ascii_digit() {
                let start = i;
                i += 1;
                while i < b.len() && {
                    let d = b[i] as char;
                    d.is_ascii_digit() || d == '.'
                } {
                    i += 1;
                }
                toks.push(d[start..i].to_string());
            } else {
                i += 1;
            }
        }
        let mut out = Vec::new();
        let (mut cx, mut cy, mut sx, mut sy) = (0.0, 0.0, 0.0, 0.0);
        let mut j = 0;
        let mut cmd = ' ';
        let num = |j: &mut usize| -> f64 {
            let v = toks[*j].parse().unwrap();
            *j += 1;
            v
        };
        while j < toks.len() {
            if toks[j].chars().next().unwrap().is_ascii_alphabetic() {
                cmd = toks[j].chars().next().unwrap();
                j += 1;
            }
            let rel = cmd.is_ascii_lowercase();
            match cmd.to_ascii_uppercase() {
                'M' => {
                    let (mut x, mut y) = (num(&mut j), num(&mut j));
                    if rel {
                        x += cx;
                        y += cy;
                    }
                    cx = x;
                    cy = y;
                    sx = x;
                    sy = y;
                    out.push((cx, cy));
                    cmd = if rel { 'l' } else { 'L' };
                }
                'L' => {
                    let (mut x, mut y) = (num(&mut j), num(&mut j));
                    if rel {
                        x += cx;
                        y += cy;
                    }
                    cx = x;
                    cy = y;
                    out.push((cx, cy));
                }
                'H' => {
                    let mut x = num(&mut j);
                    if rel {
                        x += cx;
                    }
                    cx = x;
                    out.push((cx, cy));
                }
                'V' => {
                    let mut y = num(&mut j);
                    if rel {
                        y += cy;
                    }
                    cy = y;
                    out.push((cx, cy));
                }
                'Z' => {
                    cx = sx;
                    cy = sy;
                }
                _ => unreachable!(),
            }
        }
        out
    }

    #[test]
    fn relative_and_absolute_encode_same_geometry() {
        let shape = holed_shape();
        let abs = SvgWriter {
            relative: false,
            shorthands: false,
            precision: Some(2),
        }
        .encode_path(&shape);
        for shorthands in [false, true] {
            let rel = SvgWriter {
                relative: true,
                shorthands,
                precision: Some(2),
            }
            .encode_path(&shape);
            assert_eq!(
                parse_abs(&abs),
                parse_abs(&rel),
                "relative (shorthands={shorthands}) geometry diverges from absolute:\n abs={abs}\n rel={rel}"
            );
        }
    }
}
