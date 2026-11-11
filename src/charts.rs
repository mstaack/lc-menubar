//! Chart rendering for the menu (tiny-skia → PNG). Images are drawn at 2× for Retina
//! and use colors that read on both light and dark menu backgrounds.

use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

pub const SCALE: f32 = 2.0;

#[derive(Clone, Copy)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    pub const fn with_alpha(self, a: u8) -> Rgba {
        Rgba(self.0, self.1, self.2, a)
    }
}

use std::sync::atomic::{AtomicBool, Ordering};
static DARK: AtomicBool = AtomicBool::new(false);

/// Charts are bitmaps, so pick the palette for the current appearance before drawing.
pub fn set_dark(dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
}

fn dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

pub fn is_dark_palette() -> bool {
    dark()
}

/// Chart hues, tuned so lines stay vivid on light glass and do not glare on dark.
#[derive(Clone, Copy)]
pub enum Hue {
    Blue,
    Teal,
}

impl Hue {
    pub fn rgba(self) -> Rgba {
        match (self, dark()) {
            (Hue::Blue, false) => Rgba(37, 99, 235, 255),
            (Hue::Blue, true) => Rgba(96, 165, 250, 255),
            (Hue::Teal, false) => Rgba(13, 148, 136, 255),
            (Hue::Teal, true) => Rgba(45, 212, 191, 255),
        }
    }
}
pub const NEUTRAL: Rgba = Rgba(128, 128, 136, 255);
pub const POSITIVE: Rgba = Rgba(76, 195, 138, 255);
pub const WARNING: Rgba = Rgba(224, 162, 58, 255);
pub const NEGATIVE: Rgba = Rgba(229, 72, 77, 255);

fn paint(c: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c.0, c.1, c.2, c.3);
    p.anti_alias = true;
    p
}

fn canvas(w_pt: f32, h_pt: f32) -> Option<Pixmap> {
    Pixmap::new((w_pt * SCALE).round() as u32, (h_pt * SCALE).round() as u32)
}

fn png(p: Pixmap) -> Option<Vec<u8>> {
    p.encode_png().ok()
}

/// Muted horizontal grid lines at 0, 50 and 100 % of the chart height.
fn grid(p: &mut Pixmap, w: f32, h: f32, top: f32, bottom: f32) {
    let st = Stroke {
        width: 1.0,
        ..Default::default()
    };
    {
        let frac = 1.0f32;
        let y = (top + (bottom - top) * frac).round() + 0.5;
        let mut pb = PathBuilder::new();
        pb.move_to(0.0, y);
        pb.line_to(w, y);
        if let Some(path) = pb.finish() {
            let alpha = if frac == 1.0 { 70 } else { 36 };
            p.stroke_path(
                &path,
                &paint(NEUTRAL.with_alpha(alpha)),
                &st,
                Transform::identity(),
                None,
            );
        }
    }
    let _ = h;
}

/// Downsample to at most `n` points by bucket average (keeps peaks visible enough
/// at menu sizes, and smooths noisy per-minute data).
pub fn resample(points: &[f64], n: usize) -> Vec<f64> {
    if points.len() <= n || n == 0 {
        return points.to_vec();
    }
    (0..n)
        .map(|i| {
            let a = i * points.len() / n;
            let b = ((i + 1) * points.len() / n).max(a + 1);
            points[a..b].iter().sum::<f64>() / (b - a) as f64
        })
        .collect()
}

/// Smooth line chart with a soft area fill. The first series is the accent; any
/// further series are drawn quietly in neutral. The y-scale runs from 0 to `floor`
/// (100 for percentages, as in Laravel Cloud's dashboard), and grows past it only
/// when the data does.
pub fn line_chart(
    series: &[Vec<f64>],
    hue: Hue,
    floor: f64,
    w_pt: f32,
    h_pt: f32,
) -> Option<Vec<u8>> {
    let mut p = canvas(w_pt, h_pt)?;
    let (w, h) = (p.width() as f32, p.height() as f32);
    let (top, bottom) = (3.0 * SCALE, h - 1.0 * SCALE);
    grid(&mut p, w, h, top, bottom);

    let data_max = series.iter().flatten().copied().fold(0.0f64, f64::max);
    let y_max = data_max.max(floor).max(1e-9);

    // Draw neutral series first so the accent sits on top.
    for (idx, raw) in series.iter().enumerate().rev() {
        let pts = resample(raw, 120);
        if pts.len() < 2 {
            continue;
        }
        let color = if idx == 0 {
            hue.rgba()
        } else {
            NEUTRAL.with_alpha(170)
        };
        let xy: Vec<(f32, f32)> = pts
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let x = i as f32 / (pts.len() - 1) as f32 * (w - 2.0 * SCALE) + SCALE;
                let y = bottom - (v / y_max) as f32 * (bottom - top);
                (x, y.clamp(top, bottom))
            })
            .collect();

        let line = smooth_path(&xy);
        if idx == 0 {
            // Area fill under the accent line, fading toward the baseline.
            let mut fill = smooth_builder(&xy);
            fill.line_to(xy.last().unwrap().0, bottom);
            fill.line_to(xy[0].0, bottom);
            fill.close();
            if let Some(area) = fill.finish() {
                let mut paint_fill = Paint {
                    anti_alias: true,
                    ..Default::default()
                };
                paint_fill.shader = tiny_skia::LinearGradient::new(
                    tiny_skia::Point::from_xy(0.0, top),
                    tiny_skia::Point::from_xy(0.0, bottom),
                    vec![
                        tiny_skia::GradientStop::new(
                            0.0,
                            tiny_skia::Color::from_rgba8(
                                color.0,
                                color.1,
                                color.2,
                                if dark() { 80 } else { 90 },
                            ),
                        ),
                        tiny_skia::GradientStop::new(
                            1.0,
                            tiny_skia::Color::from_rgba8(color.0, color.1, color.2, 0),
                        ),
                    ],
                    tiny_skia::SpreadMode::Pad,
                    Transform::identity(),
                )
                .unwrap_or(tiny_skia::Shader::SolidColor(tiny_skia::Color::TRANSPARENT));
                p.fill_path(
                    &area,
                    &paint_fill,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
        if let Some(path) = line {
            let st = Stroke {
                width: if idx == 0 { 1.5 * SCALE } else { 1.0 * SCALE },
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Default::default()
            };
            p.stroke_path(&path, &paint(color), &st, Transform::identity(), None);
        }
        if idx == 0 {
            let (x, y) = *xy.last().unwrap();
            if let Some(dot) = PathBuilder::from_circle(x - 0.5 * SCALE, y, 2.25 * SCALE) {
                p.fill_path(
                    &dot,
                    &paint(color),
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
    }
    png(p)
}

fn smooth_builder(xy: &[(f32, f32)]) -> PathBuilder {
    // Catmull-Rom → cubic Bézier, tension 0.5 (gentle, does not overshoot much).
    let mut pb = PathBuilder::new();
    pb.move_to(xy[0].0, xy[0].1);
    for i in 0..xy.len() - 1 {
        let p0 = xy[i.saturating_sub(1)];
        let p1 = xy[i];
        let p2 = xy[i + 1];
        let p3 = xy[(i + 2).min(xy.len() - 1)];
        let t = 1.0 / 6.0;
        let c1 = (p1.0 + (p2.0 - p0.0) * t, p1.1 + (p2.1 - p0.1) * t);
        let c2 = (p2.0 - (p3.0 - p1.0) * t, p2.1 - (p3.1 - p1.1) * t);
        pb.cubic_to(c1.0, c1.1, c2.0, c2.1, p2.0, p2.1);
    }
    pb
}

fn smooth_path(xy: &[(f32, f32)]) -> Option<tiny_skia::Path> {
    smooth_builder(xy).finish()
}

/// Request volume as thin bars: total in neutral, the 5xx share stacked on top in red.
pub fn bar_chart(total: &[f64], errors: &[f64], w_pt: f32, h_pt: f32) -> Option<Vec<u8>> {
    let mut p = canvas(w_pt, h_pt)?;
    let (w, h) = (p.width() as f32, p.height() as f32);
    let (top, bottom) = (3.0 * SCALE, h - 1.0 * SCALE);
    grid(&mut p, w, h, top, bottom);

    let n_max = ((w_pt / 7.0) as usize).max(8);
    let bucket = |v: &[f64]| -> Vec<f64> {
        if v.len() <= n_max {
            return v.to_vec();
        }
        (0..n_max)
            .map(|i| {
                let a = i * v.len() / n_max;
                let b = ((i + 1) * v.len() / n_max).max(a + 1);
                v[a..b].iter().sum()
            })
            .collect()
    };
    let t = bucket(total);
    let e = bucket(errors);
    if t.is_empty() {
        return png(p);
    }
    let y_max = t.iter().copied().fold(0.0f64, f64::max).max(1.0) * 1.1;
    let slot = w / t.len() as f32;
    let bar = (slot * 0.62).max(1.0 * SCALE);
    for (i, v) in t.iter().enumerate() {
        let x = i as f32 * slot + (slot - bar) / 2.0;
        let hh =
            ((v / y_max) as f32 * (bottom - top)).max(if *v > 0.0 { 1.0 * SCALE } else { 0.0 });
        if let Some(r) = Rect::from_xywh(x, bottom - hh, bar, hh) {
            p.fill_rect(
                r,
                &paint(Hue::Blue.rgba().with_alpha(if dark() { 150 } else { 120 })),
                Transform::identity(),
                None,
            );
        }
        let ev = e.get(i).copied().unwrap_or(0.0);
        if ev > 0.0 {
            let eh = ((ev / y_max) as f32 * (bottom - top))
                .max(1.5 * SCALE)
                .min(hh.max(1.5 * SCALE));
            if let Some(r) = Rect::from_xywh(x, bottom - eh, bar, eh) {
                p.fill_rect(r, &paint(NEGATIVE), Transform::identity(), None);
            }
        }
    }
    png(p)
}

/// Horizontal meter (e.g. bandwidth allowance). Turns amber above 80 %, red above 95 %.
pub fn meter(fraction: f64, w_pt: f32, h_pt: f32) -> Option<Vec<u8>> {
    let mut p = canvas(w_pt, h_pt)?;
    let (w, h) = (p.width() as f32, p.height() as f32);
    let radius = h / 2.0;
    let track = rounded_rect(0.0, 0.0, w, h, radius)?;
    p.fill_path(
        &track,
        &paint(NEUTRAL.with_alpha(55)),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    let f = fraction.clamp(0.0, 1.0) as f32;
    if f > 0.0 {
        let color = if f > 0.95 {
            NEGATIVE
        } else if f > 0.8 {
            WARNING
        } else {
            Hue::Blue.rgba()
        };
        if let Some(fill) = rounded_rect(0.0, 0.0, (w * f).max(h), h, radius) {
            p.fill_path(
                &fill,
                &paint(color),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
    png(p)
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let k = 0.552_284_8 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Menu row icon: a status dot followed by a tiny CPU trace (or blank space, so
/// titles stay aligned). Returned as RGBA for `muda::Icon` at 2× (≤ 18 pt tall).
pub fn row_icon(status: Rgba, cpu: Option<&[f64]>) -> (Vec<u8>, u32, u32) {
    let (w_pt, h_pt) = (44.0f32, 14.0f32);
    let mut p = canvas(w_pt, h_pt).expect("pixmap");
    let (w, h) = (p.width() as f32, p.height() as f32);
    if let Some(dot) = PathBuilder::from_circle(4.0 * SCALE, h / 2.0, 3.5 * SCALE) {
        p.fill_path(
            &dot,
            &paint(status),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    if let Some(cpu) = cpu.filter(|c| c.len() >= 2) {
        let pts = resample(cpu, 24);
        let (x0, x1) = (12.0 * SCALE, w - 1.0 * SCALE);
        let (top, bottom) = (2.0 * SCALE, h - 2.0 * SCALE);
        let y_max = pts.iter().copied().fold(0.0f64, f64::max).max(25.0);
        let xy: Vec<(f32, f32)> = pts
            .iter()
            .enumerate()
            .map(|(i, v)| {
                (
                    x0 + i as f32 / (pts.len() - 1) as f32 * (x1 - x0),
                    bottom - (v / y_max) as f32 * (bottom - top),
                )
            })
            .collect();
        if let Some(path) = smooth_path(&xy) {
            let st = Stroke {
                width: 1.25 * SCALE,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Default::default()
            };
            p.stroke_path(
                &path,
                &paint(NEUTRAL.with_alpha(220)),
                &st,
                Transform::identity(),
                None,
            );
        }
    }
    straight_rgba(&p)
}

/// muda icons expect straight (non-premultiplied) RGBA.
fn straight_rgba(p: &Pixmap) -> (Vec<u8>, u32, u32) {
    let data = p
        .pixels()
        .iter()
        .flat_map(|px| {
            let c = px.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    (data, p.width(), p.height())
}

/// Small status dot as PNG (8 pt at 2×) for card headings.
pub fn status_png(color: Rgba) -> Option<Vec<u8>> {
    let mut p = canvas(8.0, 8.0)?;
    let c = p.width() as f32 / 2.0;
    let dot = PathBuilder::from_circle(c, c, 3.5 * SCALE)?;
    p.fill_path(
        &dot,
        &paint(color),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    png(p)
}

/// Where the status dot sits on the 22×16 pt cloud icon (center, radius), in points.
pub const ICON_DOT: (f32, f32, f32) = (17.6, 12.2, 3.0);

/// The cloud as a template image (PNG): macOS tints it for the menu bar. With
/// `gap`, a ring is cleared where the status dot goes.
pub fn cloud_template_png(gap: bool) -> Option<Vec<u8>> {
    let (rgba, w, h) = status_icon(false, None, gap);
    let mut p = Pixmap::new(w, h)?;
    for (dst, src) in p.pixels_mut().iter_mut().zip(rgba.as_chunks::<4>().0) {
        *dst = tiny_skia::ColorU8::from_rgba(src[0], src[1], src[2], src[3]).premultiply();
    }
    png(p)
}

/// The status dot alone (PNG, 6 pt at 2×).
pub fn dot_png(color: Rgba) -> Option<Vec<u8>> {
    let r = ICON_DOT.2;
    let mut p = canvas(2.0 * r, 2.0 * r)?;
    let d = PathBuilder::from_circle(r * SCALE, r * SCALE, r * SCALE)?;
    p.fill_path(
        &d,
        &paint(color),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    png(p)
}

/// App icon (Finder, login items) as a `px`×`px` PNG: a white cloud with a green
/// status dot on a dark rounded square, following Apple's icon grid (the body
/// is 824/1024 of the canvas).
pub fn app_icon_png(px: u32) -> Option<Vec<u8>> {
    let mut p = Pixmap::new(px, px)?;
    let k = px as f32 / 1024.0;
    let body = rounded_rect(100.0 * k, 100.0 * k, 824.0 * k, 824.0 * k, 185.0 * k)?;
    let mut bg = Paint {
        anti_alias: true,
        ..Default::default()
    };
    bg.shader = tiny_skia::LinearGradient::new(
        tiny_skia::Point::from_xy(0.0, 100.0 * k),
        tiny_skia::Point::from_xy(0.0, 924.0 * k),
        vec![
            tiny_skia::GradientStop::new(0.0, tiny_skia::Color::from_rgba8(58, 64, 76, 255)),
            tiny_skia::GradientStop::new(1.0, tiny_skia::Color::from_rgba8(24, 27, 33, 255)),
        ],
        tiny_skia::SpreadMode::Pad,
        Transform::identity(),
    )?;
    p.fill_path(&body, &bg, FillRule::Winding, Transform::identity(), None);
    // The menu bar cloud's geometry (22×16 pt), scaled into the body, with a
    // flat base between the outer puffs so the outline stays clean when large.
    let s = 30.0 * k;
    let (ox, oy) = (512.0 * k - 11.3 * s, 512.0 * k - 9.6 * s);
    let mut pb = PathBuilder::new();
    for (cx, cy, r) in [(6.6, 9.4, 3.6), (11.0, 6.9, 4.7), (15.7, 9.4, 3.6)] {
        pb.push_circle(ox + cx * s, oy + cy * s, r * s);
    }
    pb.push_rect(tiny_skia::Rect::from_xywh(
        ox + 6.6 * s,
        oy + 8.6 * s,
        9.1 * s,
        4.4 * s,
    )?);
    let cloud = pb.finish()?;
    // Cloud on its own layer, so the gap around the dot is cut from the cloud
    // only and the background shows through untouched.
    let mut layer = Pixmap::new(px, px)?;
    layer.fill_path(
        &cloud,
        &paint(Rgba(246, 247, 249, 255)),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    let (cx, cy) = (ox + ICON_DOT.0 * s, oy + ICON_DOT.1 * s);
    let clear = Paint {
        anti_alias: true,
        blend_mode: tiny_skia::BlendMode::Clear,
        ..Default::default()
    };
    let gap = PathBuilder::from_circle(cx, cy, (ICON_DOT.2 + 1.4) * s)?;
    layer.fill_path(&gap, &clear, FillRule::Winding, Transform::identity(), None);
    p.draw_pixmap(
        0,
        0,
        layer.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    let dot = PathBuilder::from_circle(cx, cy, ICON_DOT.2 * s)?;
    p.fill_path(
        &dot,
        &paint(Rgba(52, 199, 89, 255)),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    png(p)
}

/// Menu bar icon: a cloud outline with a status dot cut into its lower right.
/// RGBA at 2× (22×16 pt). `dot` is `None` without data; `gap` clears room for a
/// dot drawn separately.
pub fn status_icon(dark_bar: bool, dot: Option<Rgba>, gap: bool) -> (Vec<u8>, u32, u32) {
    let (w_pt, h_pt) = (22.0f32, 16.0f32);
    let mut p = canvas(w_pt, h_pt).expect("pixmap");
    let s = SCALE;
    let ink = if dark_bar {
        Rgba(255, 255, 255, 235)
    } else {
        Rgba(0, 0, 0, 220)
    };
    // Cloud = union of three puffs and a rounded base. Filling it, then clearing
    // the same shape shrunk by the stroke width, leaves a clean union outline.
    let shape = |inset: f32| -> Option<tiny_skia::Path> {
        let mut pb = PathBuilder::new();
        for (cx, cy, r) in [(6.6, 9.4, 3.6), (11.0, 6.9, 4.7), (15.7, 9.0, 3.6)] {
            pb.push_circle(cx * s, cy * s, (r - inset) * s);
        }
        let (x0, y0, x1, y1) = (5.2 + inset, 8.6, 17.4 - inset, 13.0 - inset);
        let r = (2.2 - inset).max(0.5);
        if let Some(rr) = rounded_rect(x0 * s, y0 * s, (x1 - x0) * s, (y1 - y0) * s, r * s) {
            pb.push_path(&rr);
        }
        pb.finish()
    };
    let stroke = 1.4f32;
    if let Some(outer) = shape(0.0) {
        p.fill_path(
            &outer,
            &paint(ink),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    let clear = Paint {
        anti_alias: true,
        blend_mode: tiny_skia::BlendMode::Clear,
        ..Default::default()
    };
    if let Some(inner) = shape(stroke) {
        p.fill_path(
            &inner,
            &clear,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    let (cx, cy) = (ICON_DOT.0 * s, ICON_DOT.1 * s);
    if gap || dot.is_some() {
        // Gap around the dot so it reads as a separate mark.
        if let Some(g) = PathBuilder::from_circle(cx, cy, (ICON_DOT.2 + 1.4) * s) {
            p.fill_path(&g, &clear, FillRule::Winding, Transform::identity(), None);
        }
    }
    if let Some(c) = dot
        && let Some(d) = PathBuilder::from_circle(cx, cy, ICON_DOT.2 * s)
    {
        p.fill_path(
            &d,
            &paint(c),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    straight_rgba(&p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_size(png: &[u8]) -> (u32, u32) {
        // PNG IHDR: width/height are big-endian u32 at byte 16 and 20.
        (
            u32::from_be_bytes(png[16..20].try_into().unwrap()),
            u32::from_be_bytes(png[20..24].try_into().unwrap()),
        )
    }

    #[test]
    fn renders_at_2x() {
        let png = line_chart(&[vec![1.0, 5.0, 3.0, 8.0]], Hue::Blue, 1.0, 260.0, 40.0).unwrap();
        assert_eq!(decode_size(&png), (520, 80));
        let png = bar_chart(&[10.0, 20.0, 5.0], &[0.0, 2.0, 0.0], 260.0, 32.0).unwrap();
        assert_eq!(decode_size(&png), (520, 64));
        let png = meter(0.42, 260.0, 6.0).unwrap();
        assert_eq!(decode_size(&png), (520, 12));
    }

    #[test]
    fn handles_empty_and_flat_data() {
        assert!(line_chart(&[vec![]], Hue::Blue, 1.0, 100.0, 20.0).is_some());
        assert!(line_chart(&[vec![0.0, 0.0, 0.0]], Hue::Teal, 0.0, 100.0, 20.0).is_some());
        assert!(bar_chart(&[], &[], 100.0, 20.0).is_some());
    }

    #[test]
    fn resample_averages_buckets() {
        assert_eq!(resample(&[1.0, 3.0, 5.0, 7.0], 2), vec![2.0, 6.0]);
        assert_eq!(resample(&[1.0, 2.0], 10), vec![1.0, 2.0]);
    }

    #[test]
    fn status_icon_has_ink_and_dot() {
        let (rgba, w, h) = status_icon(false, Some(POSITIVE), true);
        assert_eq!((w, h), (44, 32));
        let px = |x: u32, y: u32| &rgba[((y * w + x) * 4) as usize..((y * w + x) * 4 + 4) as usize];
        // Dot center is green and opaque.
        let d = px((17.6 * 2.0) as u32, (12.2 * 2.0) as u32);
        assert!(d[1] > d[0] && d[3] > 200, "dot pixel {d:?}");
        // Inside the cloud (not on the outline) is transparent.
        assert_eq!(px(22, 18)[3], 0);
    }

    #[test]
    fn row_icon_fits_menu_height() {
        let (rgba, w, h) = row_icon(POSITIVE, Some(&[1.0, 2.0, 3.0]));
        assert_eq!((w, h), (88, 28));
        assert_eq!(rgba.len(), (w * h * 4) as usize);
    }
}
