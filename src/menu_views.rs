//! Rich, non-interactive content inside the native menu (the iStat Menus approach):
//! placeholder items are added with muda, then the NSMenuItems that are actually on
//! screen are found by walking from the status item's NSMenu (muda keeps a separate
//! NSMenu per parent for submenus, so a submenu's own handle is not the one shown)
//! and get custom views, images and attributed titles.

use crate::charts::Rgba;

/// Width of custom views; the menu stretches them to its own width.
pub const CARD_W: f64 = 340.0;
/// About how many characters fit in an environment row before its right column.
const ROW_CHARS: usize = 41;
/// Horizontal inset matching native menu item text.
pub const INSET: f64 = 14.0;

#[derive(Clone, Copy, PartialEq)]
pub enum Tone {
    Primary,
    Secondary,
    Muted,
    Mono,
    Negative,
}

pub enum Card {
    /// Small section heading, like the app name above its environments.
    Heading { text: String },
    /// A status line with a colored dot, then a few lines of text.
    Status {
        title: String,
        dot: Option<Rgba>,
        lines: Vec<(String, Tone)>,
    },
    /// Title + big current value on one line, chart below, caption under it.
    /// `dim` marks data from another range while the selected one loads.
    Chart {
        title: String,
        value: String,
        caption: String,
        png: Vec<u8>,
        chart_h: f64,
        dim: bool,
    },
    /// Columns of small label over value.
    Stats { items: Vec<(String, String)> },
    /// Label, big amount, optional meter and caption.
    Spend {
        title: String,
        value: String,
        caption: String,
        meter: Option<Vec<u8>>,
    },
    /// Segmented switch (time range, organization). Clicking keeps the menu open.
    Segments {
        kind: Segment,
        title: String,
        labels: Vec<String>,
        selected: usize,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Segment {
    Range = 0,
    Org = 1,
}

impl Card {
    /// Content fingerprint: equal cards can reuse the same native view.
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let rgba = |c: &Option<Rgba>, h: &mut std::collections::hash_map::DefaultHasher| {
            c.map(|c| (c.0, c.1, c.2, c.3)).hash(h)
        };
        match self {
            Card::Heading { text } => (0u8, text).hash(&mut h),
            Card::Status { title, dot, lines } => {
                (1u8, title).hash(&mut h);
                rgba(dot, &mut h);
                for (t, tone) in lines {
                    (t, *tone as u8).hash(&mut h);
                }
            }
            Card::Chart {
                title,
                value,
                caption,
                png,
                chart_h,
                dim,
            } => (2u8, title, value, caption, png, chart_h.to_bits(), dim).hash(&mut h),
            Card::Stats { items } => (3u8, items).hash(&mut h),
            Card::Spend {
                title,
                value,
                caption,
                meter,
            } => (4u8, title, value, caption, meter).hash(&mut h),
            Card::Segments {
                kind,
                title,
                labels,
                selected,
            } => (5u8, *kind as u8, title, labels, selected).hash(&mut h),
        }
        h.finish()
    }
}

/// A standard (clickable) row restyled natively: dot image + name, meta and a
/// right-aligned value column.
pub struct Row {
    pub dot: Rgba,
    pub name: String,
    pub meta: String,
    pub right: String,
}

/// Item path from the status item's menu: [top index] or [top index, sub index].
pub type Path = Vec<usize>;

#[cfg(target_os = "macos")]
pub use imp::{
    alert, apply, ask, confirm, copy_text, force_appearance, forget_views, is_dark, popup, prompt,
    render_png, set_range_handler, set_status_icon,
};

#[cfg(not(target_os = "macos"))]
pub fn apply(
    _top: *mut std::ffi::c_void,
    _cards: &[(Path, Card)],
    _rows: &[(Path, Row)],
    _hidden: &[(Path, bool)],
) {
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{NSControlSize, NSSegmentSwitchTracking, NSSegmentedControl};
    use objc2_foundation::NSObject;
    use std::cell::RefCell;

    /// The changing parts of a chart card, to update it without rebuilding.
    struct ChartParts {
        title: String,
        value: Retained<NSTextField>,
        caption: Retained<NSTextField>,
        image: Retained<NSImageView>,
        size: (f64, f64),
    }

    type SegmentHandler = Box<dyn Fn(Segment, usize)>;

    thread_local! {
        /// Chart cards by their view's address.
        static CHART_PARTS: RefCell<std::collections::HashMap<usize, ChartParts>> = RefCell::new(Default::default());
        /// Views built for the previous apply, by card fingerprint, for reuse. Several
        /// items can show identical cards, and a view can only be in one item.
        static VIEW_CACHE: RefCell<std::collections::HashMap<u64, Vec<Retained<NSView>>>> = RefCell::new(Default::default());
        static RANGE_HANDLER: RefCell<Option<SegmentHandler>> = RefCell::new(None);
        /// Controls hold their target weakly; keep the current ones alive here.
        static TARGETS: RefCell<Vec<Retained<RangeTarget>>> = const { RefCell::new(Vec::new()) };
    }

    /// Called with the control kind and selected segment when a switch changes.
    pub fn set_range_handler(f: impl Fn(Segment, usize) + 'static) {
        RANGE_HANDLER.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements; no Drop impl.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LcmSegmentTarget"]
        struct RangeTarget;

        impl RangeTarget {
            #[unsafe(method(rangeChanged:))]
            fn range_changed(&self, sender: &NSSegmentedControl) {
                let i = sender.selectedSegment().max(0) as usize;
                let kind = if sender.tag() == Segment::Org as isize { Segment::Org } else { Segment::Range };
                RANGE_HANDLER.with(|h| {
                    if let Some(f) = &*h.borrow() {
                        f(kind, i);
                    }
                });
            }
        }
    );

    thread_local! {
        static PENDING_POPUP: RefCell<Option<(Retained<NSMenu>, f64, f64)>> = const { RefCell::new(None) };
        static POPPER: RefCell<Option<Retained<Popper>>> = const { RefCell::new(None) };
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements; no Drop impl.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LcmPopper"]
        struct Popper;

        impl Popper {
            #[unsafe(method(pop))]
            fn pop(&self) {
                if let Some((menu, x, y)) = PENDING_POPUP.with(|p| p.borrow_mut().take()) {
                    menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(x, y), None);
                }
            }
        }
    );

    impl RangeTarget {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            unsafe { msg_send![Self::alloc(mtm), init] }
        }
    }
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSColor, NSFont, NSFontAttributeName, NSFontWeightMedium,
        NSFontWeightRegular, NSFontWeightSemibold, NSForegroundColorAttributeName, NSImage,
        NSImageScaling, NSImageView, NSLineBreakMode, NSMenu, NSMenuItem, NSMutableParagraphStyle,
        NSParagraphStyleAttributeName, NSTextAlignment, NSTextField, NSTextTab, NSView,
    };
    use objc2_foundation::{
        NSArray, NSAttributedString, NSData, NSDictionary, NSMutableAttributedString, NSPoint,
        NSRect, NSSize, NSString,
    };

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    fn tone_color(tone: Tone) -> Retained<NSColor> {
        match tone {
            Tone::Primary => NSColor::labelColor(),
            Tone::Secondary | Tone::Mono => NSColor::secondaryLabelColor(),
            Tone::Muted => NSColor::tertiaryLabelColor(),
            Tone::Negative => NSColor::systemRedColor(),
        }
    }

    fn font(size: f64, weight: f64, mono: bool) -> Retained<NSFont> {
        if mono {
            NSFont::monospacedSystemFontOfSize_weight(size, weight)
        } else {
            NSFont::monospacedDigitSystemFontOfSize_weight(size, weight)
        }
    }

    fn label(
        mtm: MainThreadMarker,
        text: &str,
        size: f64,
        tone: Tone,
        weight: f64,
    ) -> Retained<NSTextField> {
        let tf = NSTextField::labelWithString(&NSString::from_str(text), mtm);
        tf.setFont(Some(&font(size, weight, tone == Tone::Mono)));
        tf.setTextColor(Some(&tone_color(tone)));
        tf.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        tf
    }

    fn image(png: &[u8], w: f64, h: f64) -> Option<Retained<NSImage>> {
        let img = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(png))?;
        img.setSize(NSSize::new(w, h));
        Some(img)
    }

    fn image_view(
        mtm: MainThreadMarker,
        png: &[u8],
        w: f64,
        h: f64,
    ) -> Option<Retained<NSImageView>> {
        let img = image(png, w, h)?;
        let iv = NSImageView::imageViewWithImage(&img, mtm);
        iv.setImageScaling(NSImageScaling::ScaleAxesIndependently);
        Some(iv)
    }

    /// Snap to the 2x pixel grid so edges and text land on whole device pixels.
    fn snap(v: f64) -> f64 {
        (v * 2.0).round() / 2.0
    }

    /// Natural height and first-baseline offset (from the top) of a view.
    fn metrics(v: &NSView) -> (f64, f64) {
        (
            v.intrinsicContentSize().height,
            v.firstBaselineOffsetFromTop(),
        )
    }

    /// Top offsets that put every view's first baseline on one line, and the row height.
    fn baseline_row(views: &[&NSView]) -> (Vec<f64>, f64) {
        let m: Vec<(f64, f64)> = views.iter().map(|v| metrics(v)).collect();
        let baseline = m.iter().map(|(_, b)| *b).fold(0.0, f64::max);
        let tops: Vec<f64> = m.iter().map(|(_, b)| snap(baseline - b)).collect();
        let height = m
            .iter()
            .zip(&tops)
            .map(|((h, _), t)| t + h)
            .fold(0.0, f64::max);
        (tops, snap(height))
    }

    #[derive(Clone, Copy)]
    enum Anchor {
        Left,
        Right,
        Stretch,
    }

    /// Positions children from the top down (AppKit's origin is bottom-left) and
    /// sets autoresizing so the view stretches with the menu.
    struct Layout {
        mtm: MainThreadMarker,
        views: Vec<(Retained<NSView>, f64, f64, f64, f64, Anchor)>,
        y: f64,
    }

    impl Layout {
        fn new(mtm: MainThreadMarker, top_pad: f64) -> Self {
            Self {
                mtm,
                views: vec![],
                y: top_pad,
            }
        }
        fn inner_w() -> f64 {
            CARD_W - 2.0 * INSET
        }
        fn put(&mut self, v: Retained<NSView>, x: f64, top: f64, w: f64, h: f64, a: Anchor) {
            self.views.push((v, x, top, w, h, a));
        }
        fn text(&mut self, text: &str, size: f64, tone: Tone, weight: f64, h: f64) {
            let l = label(self.mtm, text, size, tone, weight);
            let y = self.y;
            self.put(
                l.into_super().into_super(),
                INSET,
                y,
                Self::inner_w(),
                h,
                Anchor::Stretch,
            );
            self.y += h;
        }
        fn finish(self, bottom_pad: f64) -> Retained<NSView> {
            let height = self.y + bottom_pad;
            let view =
                NSView::initWithFrame(NSView::alloc(self.mtm), rect(0.0, 0.0, CARD_W, height));
            view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
            for (v, x, top, w, h, a) in self.views {
                v.setFrame(rect(x, height - top - h, w, h));
                v.setAutoresizingMask(match a {
                    Anchor::Left => NSAutoresizingMaskOptions::ViewMaxXMargin,
                    Anchor::Right => NSAutoresizingMaskOptions::ViewMinXMargin,
                    Anchor::Stretch => NSAutoresizingMaskOptions::ViewWidthSizable,
                });
                view.addSubview(&v);
            }
            view
        }
    }

    /// Hairline above a card, separating it from the previous one in the same
    /// submenu. Content sits directly on the menu material, like native items.
    fn divider(mtm: MainThreadMarker, view: &NSView) {
        use objc2_app_kit::{NSBox, NSBoxType, NSTitlePosition};
        let frame = view.frame();
        let b = NSBox::new(mtm);
        b.setBoxType(NSBoxType::Custom);
        b.setTitlePosition(NSTitlePosition::NoTitle);
        b.setBorderWidth(0.0);
        b.setFillColor(&NSColor::separatorColor());
        b.setFrame(rect(
            INSET,
            frame.size.height - 1.0,
            frame.size.width - 2.0 * INSET,
            1.0,
        ));
        b.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
        );
        view.addSubview(&b);
    }

    fn build(mtm: MainThreadMarker, card: &Card) -> Retained<NSView> {
        let (regular, medium, semibold) = unsafe {
            (
                NSFontWeightRegular,
                NSFontWeightMedium,
                NSFontWeightSemibold,
            )
        };
        let w = Layout::inner_w();
        match card {
            Card::Heading { text } => {
                let mut l = Layout::new(mtm, 6.0);
                l.text(text, 11.0, Tone::Secondary, semibold, 14.0);
                l.finish(2.0)
            }
            Card::Status { title, dot, lines } => {
                let mut l = Layout::new(mtm, 8.0);
                let mut tx = INSET;
                let t = label(mtm, title, 13.0, Tone::Primary, medium);
                let (h, baseline) = metrics(&t);
                if let Some(png) = dot.and_then(crate::charts::status_png)
                    && let Some(iv) = image_view(mtm, &png, 8.0, 8.0)
                {
                    // Center the dot on the title's cap height, not its line box.
                    let cap = t.font().map(|f| f.capHeight()).unwrap_or(9.0);
                    let y = snap(l.y + baseline - cap / 2.0 - 4.0);
                    l.put(
                        iv.into_super().into_super(),
                        INSET,
                        y,
                        8.0,
                        8.0,
                        Anchor::Left,
                    );
                    tx += 14.0;
                }
                let y = l.y;
                l.put(
                    t.into_super().into_super(),
                    tx,
                    y,
                    w - (tx - INSET),
                    h,
                    Anchor::Stretch,
                );
                l.y += snap(h + 2.0);
                for (text, tone) in lines {
                    let size = if *tone == Tone::Mono { 11.0 } else { 12.0 };
                    l.text(text, size, *tone, regular, 17.0);
                }
                l.finish(8.0)
            }
            Card::Chart {
                title,
                value,
                caption,
                png,
                chart_h,
                dim,
            } => {
                let mut l = Layout::new(mtm, 8.0);
                let t = label(mtm, title, 12.0, Tone::Secondary, regular);
                let v = label(mtm, value, 15.0, Tone::Primary, medium);
                v.setAlignment(NSTextAlignment::Right);
                let (tops, row_h) = baseline_row(&[&t, &v]);
                let (th, vh) = (metrics(&t).0, metrics(&v).0);
                let y = l.y;
                l.put(
                    t.into_super().into_super(),
                    INSET,
                    y + tops[0],
                    w * 0.5,
                    th,
                    Anchor::Left,
                );
                // +2: text fields inset their text 2 pt; end the digits on the chart edge.
                let value_field = v.clone();
                l.put(
                    v.into_super().into_super(),
                    INSET + w * 0.4 + 2.0,
                    y + tops[1],
                    w * 0.6,
                    vh,
                    Anchor::Right,
                );
                l.y += row_h + 6.0;
                let iv = image_view(mtm, png, w, *chart_h).unwrap_or_else(|| NSImageView::new(mtm));
                if *dim {
                    iv.setAlphaValue(0.35);
                }
                let image = iv.clone();
                let y = l.y;
                l.put(
                    iv.into_super().into_super(),
                    INSET,
                    y,
                    w,
                    *chart_h,
                    Anchor::Stretch,
                );
                l.y += chart_h + 4.0;
                let c = label(mtm, caption, 11.0, Tone::Secondary, regular);
                let caption_field = c.clone();
                let y = l.y;
                l.put(
                    c.into_super().into_super(),
                    INSET,
                    y,
                    w,
                    14.0,
                    Anchor::Stretch,
                );
                l.y += 14.0;
                let view = l.finish(8.0);
                CHART_PARTS.with(|p| {
                    p.borrow_mut().insert(
                        Retained::as_ptr(&view) as usize,
                        ChartParts {
                            title: title.clone(),
                            value: value_field,
                            caption: caption_field,
                            image,
                            size: (w, *chart_h),
                        },
                    )
                });
                view
            }
            Card::Stats { items } => {
                let mut l = Layout::new(mtm, 8.0);
                let col = w / items.len().max(1) as f64;
                let y = l.y;
                for (i, (k, v)) in items.iter().enumerate() {
                    let cx = INSET + i as f64 * col;
                    let kl = label(mtm, k, 11.0, Tone::Muted, regular);
                    l.put(
                        kl.into_super().into_super(),
                        cx,
                        y,
                        col - 8.0,
                        14.0,
                        Anchor::Left,
                    );
                    let vl = label(mtm, v, 15.0, Tone::Primary, medium);
                    l.put(
                        vl.into_super().into_super(),
                        cx,
                        y + 15.0,
                        col - 8.0,
                        20.0,
                        Anchor::Left,
                    );
                }
                l.y += 36.0;
                l.finish(8.0)
            }
            Card::Segments {
                kind,
                title,
                labels,
                selected,
            } => {
                let mut l = Layout::new(mtm, 8.0);
                let t = label(mtm, title, 12.0, Tone::Secondary, regular);
                let target = RangeTarget::new(mtm);
                let names: Vec<Retained<NSString>> =
                    labels.iter().map(|s| NSString::from_str(s)).collect();
                let control = unsafe {
                    NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                        &NSArray::from_retained_slice(&names),
                        NSSegmentSwitchTracking::SelectOne,
                        Some(&target),
                        Some(sel!(rangeChanged:)),
                        mtm,
                    )
                };
                control.setControlSize(NSControlSize::Small);
                control.setSelectedSegment(*selected as isize);
                control.setTag(*kind as isize);
                // Ranges get equal widths; organization names size to their text,
                // within the space next to the title.
                if *kind == Segment::Range {
                    for i in 0..labels.len() {
                        control.setWidth_forSegment(40.0, i as isize);
                    }
                }
                // Same font in label and control, both centered on the control's
                // height: identical metrics, so the baselines coincide exactly.
                control.setFont(t.font().as_deref());
                let (th, ch) = (metrics(&t).0, metrics(&control).0);
                let row_h = snap(ch.max(th));
                let cw = snap(control.intrinsicContentSize().width.min(w - 72.0));
                let y = l.y;
                l.put(
                    t.into_super().into_super(),
                    INSET,
                    y + snap((row_h - th) / 2.0),
                    80.0,
                    th,
                    Anchor::Left,
                );
                l.put(
                    control.into_super().into_super(),
                    INSET + w - cw,
                    y + snap((row_h - ch) / 2.0),
                    cw,
                    ch,
                    Anchor::Right,
                );
                TARGETS.with(|t| {
                    let mut t = t.borrow_mut();
                    // Old controls die with their menus; keep the list bounded.
                    if t.len() > 64 {
                        t.drain(..32);
                    }
                    t.push(target);
                });
                l.y += row_h;
                // The native separator below brings 6 pt of its own above its line;
                // 2 pt here makes the gap below equal the 8 pt above.
                l.finish(2.0)
            }
            Card::Spend {
                title,
                value,
                caption,
                meter,
            } => {
                let mut l = Layout::new(mtm, 8.0);
                l.text(title, 11.0, Tone::Secondary, regular, 14.0);
                l.text(value, 20.0, Tone::Primary, medium, 26.0);
                if let Some(png) = meter
                    && let Some(iv) = image_view(mtm, png, w, 6.0)
                {
                    let y = l.y + 4.0;
                    l.put(
                        iv.into_super().into_super(),
                        INSET,
                        y,
                        w,
                        6.0,
                        Anchor::Stretch,
                    );
                    l.y += 14.0;
                }
                l.text(caption, 11.0, Tone::Secondary, regular, 14.0);
                l.finish(8.0)
            }
        }
    }

    /// Update an existing chart card's value, caption and image in place. Returns
    /// false when `view` is not a matching chart card.
    fn update_chart(view: &NSView, card: &Card) -> bool {
        let Card::Chart {
            title,
            value,
            caption,
            png,
            chart_h,
            dim,
        } = card
        else {
            return false;
        };
        CHART_PARTS.with(|p| {
            let p = p.borrow();
            let Some(parts) = p.get(&(view as *const NSView as usize)) else {
                return false;
            };
            if parts.title != *title || parts.size.1 != *chart_h {
                return false;
            }
            let Some(img) = image(png, parts.size.0, parts.size.1) else {
                return false;
            };
            parts.value.setStringValue(&NSString::from_str(value));
            parts.caption.setStringValue(&NSString::from_str(caption));
            parts.image.setImage(Some(&img));
            parts.image.setAlphaValue(if *dim { 0.35 } else { 1.0 });
            true
        })
    }

    fn attributed(
        parts: &[(&str, Retained<NSFont>, Retained<NSColor>)],
        tab_at: Option<f64>,
    ) -> Retained<NSAttributedString> {
        let out = NSMutableAttributedString::new();
        let para = NSMutableParagraphStyle::new();
        if let Some(x) = tab_at {
            let tab = unsafe {
                NSTextTab::initWithTextAlignment_location_options(
                    NSTextTab::alloc(),
                    NSTextAlignment::Right,
                    x,
                    &NSDictionary::new(),
                )
            };
            para.setTabStops(Some(&NSArray::from_retained_slice(&[tab])));
        }
        for (text, f, c) in parts {
            let keys = unsafe {
                [
                    NSFontAttributeName,
                    NSForegroundColorAttributeName,
                    NSParagraphStyleAttributeName,
                ]
            };
            let vals: [&AnyObject; 3] = [f, c, &para];
            let dict = NSDictionary::from_slices(&keys, &vals);
            let part = unsafe {
                NSAttributedString::new_with_attributes(&NSString::from_str(text), &dict)
            };
            out.appendAttributedString(&part);
        }
        out.into_super()
    }

    /// An 8 pt status dot inline with text, vertically centered on the cap height.
    fn dot_attachment(c: Rgba, mtm: MainThreadMarker) -> Option<Retained<NSAttributedString>> {
        use objc2_app_kit::{NSAttributedStringAttachmentConveniences, NSTextAttachment};
        let img = crate::charts::status_png(c).and_then(|p| image(&p, 8.0, 8.0))?;
        let att = NSTextAttachment::new();
        att.setImage(Some(&img));
        att.setBounds(rect(0.0, 0.5, 8.0, 8.0));
        let _ = mtm;
        Some(NSAttributedString::attributedStringWithAttachment(&att))
    }

    fn style_row(item: &NSMenuItem, row: &Row, mtm: MainThreadMarker) {
        let regular = unsafe { NSFontWeightRegular };
        // The right column sits on a tab stop: keep name and meta short enough
        // not to run into it (a rough character budget; long branch names).
        let budget = ROW_CHARS
            .saturating_sub(row.name.chars().count() + row.right.chars().count() + 3)
            .max(6);
        let meta = format!("   {}", crate::model::truncate(&row.meta, budget));
        let right = format!("\t{}", row.right);
        let mut parts = vec![
            (
                row.name.as_str(),
                NSFont::menuFontOfSize(0.0),
                NSColor::labelColor(),
            ),
            (
                meta.as_str(),
                font(12.0, regular, false),
                NSColor::secondaryLabelColor(),
            ),
        ];
        if !row.right.is_empty() {
            parts.push((
                right.as_str(),
                font(12.0, regular, false),
                NSColor::tertiaryLabelColor(),
            ));
        }
        let text = attributed(&parts, Some(CARD_W - 64.0));
        let full = NSMutableAttributedString::new();
        if let Some(dot) = dot_attachment(row.dot, mtm) {
            full.appendAttributedString(&dot);
            full.appendAttributedString(&attributed(
                &[("  ", NSFont::menuFontOfSize(0.0), NSColor::labelColor())],
                None,
            ));
        }
        full.appendAttributedString(&text);
        item.setAttributedTitle(Some(&full));
    }

    fn item_at(top: &NSMenu, path: &[usize]) -> Option<Retained<NSMenuItem>> {
        let (&last, parents) = path.split_last()?;
        let mut menu: Retained<NSMenu> =
            unsafe { Retained::retain(top as *const NSMenu as *mut NSMenu) }?;
        for &i in parents {
            menu = menu.itemAtIndex(i as isize)?.submenu()?;
        }
        menu.itemAtIndex(last as isize)
    }

    /// Drop the views kept for reuse, before building a new menu.
    pub fn forget_views() {
        VIEW_CACHE.with(|c| c.borrow_mut().clear());
    }

    /// Install card views and row styles on the menu shown by the status item.
    pub fn apply(
        top: *mut std::ffi::c_void,
        cards: &[(Path, Card)],
        rows: &[(Path, Row)],
        hidden: &[(Path, bool)],
    ) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if top.is_null() {
            return;
        }
        let top: &NSMenu = unsafe { &*(top as *const NSMenu) };
        let mut old = VIEW_CACHE.with(|c| std::mem::take(&mut *c.borrow_mut()));
        let mut new_cache: std::collections::HashMap<u64, Vec<Retained<NSView>>> =
            std::collections::HashMap::new();
        let mut prev_parent: Option<usize> = None;
        for (path, card) in cards {
            // Cards inside a submenu are divided by hairlines.
            let divided = path.len() > 1 && prev_parent == Some(path[0]);
            if path.len() > 1 {
                prev_parent = Some(path[0]);
            }
            let Some(item) = item_at(top, path) else {
                continue;
            };
            let fp = card.fingerprint() ^ (divided as u64);
            // Unchanged content: keep this item's view, or move over another
            // unused one. Each cached view is taken once.
            let cached = old.get_mut(&fp).and_then(|list| {
                let cur = item.view().map(|v| Retained::as_ptr(&v));
                let i = list
                    .iter()
                    .position(|v| Some(Retained::as_ptr(v)) == cur)
                    .unwrap_or(0);
                (!list.is_empty()).then(|| list.swap_remove(i))
            });
            let view = match cached {
                Some(v) => v,
                // Same chart in the same place with new data: update its parts.
                None => match item.view().filter(|cur| update_chart(cur, card)) {
                    Some(cur) => cur,
                    None => {
                        let v = build(mtm, card);
                        if divided {
                            divider(mtm, &v);
                        }
                        v
                    }
                },
            };
            let same = item
                .view()
                .is_some_and(|cur| Retained::as_ptr(&cur) == Retained::as_ptr(&view));
            if !same {
                item.setView(Some(&view));
            }
            new_cache.entry(fp).or_default().push(view);
        }
        CHART_PARTS.with(|p| {
            let live: std::collections::HashSet<usize> = new_cache
                .values()
                .flatten()
                .map(|v| Retained::as_ptr(v) as usize)
                .collect();
            p.borrow_mut().retain(|k, _| live.contains(k));
        });
        VIEW_CACHE.with(|c| *c.borrow_mut() = new_cache);
        for (path, row) in rows {
            if let Some(item) = item_at(top, path) {
                style_row(&item, row, mtm);
            }
        }
        // Organization filter: toggling visibility keeps an open menu open.
        for (path, hide) in hidden {
            if let Some(item) = item_at(top, path) {
                item.setHidden(*hide);
            }
        }
    }

    /// Native confirmation dialog (instant, unlike spawning osascript). The
    /// destructive button comes first and is the default; Cancel maps to Escape.
    pub fn confirm(title: &str, message: &str, ok: &str) -> bool {
        ask(title, message, ok, "Cancel")
    }

    /// Two-button dialog: true when `ok` was chosen.
    pub fn ask(title: &str, message: &str, ok: &str, cancel: &str) -> bool {
        use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication};
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        NSApplication::sharedApplication(mtm).activate();
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(NSAlertStyle::Warning);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(message));
        alert.addButtonWithTitle(&NSString::from_str(ok));
        alert.addButtonWithTitle(&NSString::from_str(cancel));
        alert.runModal() == NSAlertFirstButtonReturn
    }

    /// Dialog with a text field: the entered text (trimmed), or `None` when
    /// cancelled or left empty.
    pub fn prompt(title: &str, message: &str, placeholder: &str, ok: &str) -> Option<String> {
        use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication};
        let mtm = MainThreadMarker::new()?;
        NSApplication::sharedApplication(mtm).activate();
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(message));
        alert.addButtonWithTitle(&NSString::from_str(ok));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        let field = NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
        field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
        field.setFrame(rect(0.0, 0.0, 260.0, 24.0));
        alert.setAccessoryView(Some(&field));
        alert.layout();
        alert.window().setInitialFirstResponder(Some(&field));
        if alert.runModal() != NSAlertFirstButtonReturn {
            return None;
        }
        let text = field.stringValue().to_string().trim().to_string();
        (!text.is_empty()).then_some(text)
    }

    pub fn alert(title: &str, message: &str) {
        use objc2_app_kit::{NSAlert, NSApplication};
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        NSApplication::sharedApplication(mtm).activate();
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(message));
        alert.runModal();
    }

    /// Put text on the general pasteboard (no pbcopy process).
    pub fn copy_text(text: &str) {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        unsafe { pb.setString_forType(&NSString::from_str(text), NSPasteboardTypeString) };
    }

    thread_local! {
        static STATUS_DOT: RefCell<Option<Retained<NSImageView>>> = const { RefCell::new(None) };
    }

    /// Menu bar icon: the cloud as a template image (tinted by macOS for any menu
    /// bar), plus a colored status dot drawn as a separate view over its corner.
    pub fn set_status_icon(item: &objc2_app_kit::NSStatusItem, dot: Option<Rgba>) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(button) = item.button(mtm) else {
            return;
        };
        let Some(cloud) =
            crate::charts::cloud_template_png(dot.is_some()).and_then(|p| image(&p, 22.0, 16.0))
        else {
            return;
        };
        cloud.setTemplate(true);
        button.setImage(Some(&cloud));

        let view = STATUS_DOT.with(|d| {
            d.borrow_mut()
                .get_or_insert_with(|| {
                    let v = NSImageView::new(mtm);
                    button.addSubview(&v);
                    v
                })
                .clone()
        });
        let Some(c) = dot else {
            view.setHidden(true);
            return;
        };
        let (cx, cy, r) = crate::charts::ICON_DOT;
        let Some(img) =
            crate::charts::dot_png(c).and_then(|p| image(&p, 2.0 * r as f64, 2.0 * r as f64))
        else {
            return;
        };
        view.setImage(Some(&img));
        view.setHidden(false);
        // Place the dot on the cloud wherever the button draws its image (centered
        // alone, or left of the spend title).
        let bounds = button.bounds();
        let img_rect = button
            .cell()
            .map(|cell| cell.imageRectForBounds(bounds))
            .unwrap_or(bounds);
        let sx = img_rect.size.width / 22.0;
        let sy = img_rect.size.height / 16.0;
        let x = img_rect.origin.x + (cx as f64 - r as f64) * sx;
        let y_top = (cy as f64 - r as f64) * sy;
        let y = if button.isFlipped() {
            img_rect.origin.y + y_top
        } else {
            img_rect.origin.y + img_rect.size.height - y_top - 2.0 * r as f64 * sy
        };
        view.setFrame(rect(
            snap(x),
            snap(y),
            2.0 * r as f64 * sx,
            2.0 * r as f64 * sy,
        ));
    }

    /// Whether the app currently renders in a dark appearance.
    pub fn is_dark() -> bool {
        use objc2_app_kit::{NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication};
        use objc2_foundation::NSArray;
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        let appearance = NSApplication::sharedApplication(mtm).effectiveAppearance();
        let names =
            unsafe { NSArray::from_slice(&[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]) };
        appearance
            .bestMatchFromAppearancesWithNames(&names)
            .is_some_and(|n| unsafe { &*n == NSAppearanceNameDarkAqua })
    }

    /// Development: force light or dark appearance for this app only.
    pub fn force_appearance(dark: bool) {
        use objc2_app_kit::{
            NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
        };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let name = unsafe {
            if dark {
                NSAppearanceNameDarkAqua
            } else {
                NSAppearanceNameAqua
            }
        };
        if let Some(a) = NSAppearance::appearanceNamed(name) {
            NSApplication::sharedApplication(mtm).setAppearance(Some(&a));
        }
    }

    /// Development: open the menu (or the submenu at `path`) at a screen point,
    /// without touching the mouse. Blocks until the menu is dismissed.
    pub fn popup(top: *mut std::ffi::c_void, path: &[usize], x: f64, y: f64) {
        if top.is_null() {
            return;
        }
        let top: &NSMenu = unsafe { &*(top as *const NSMenu) };
        let menu = if path.is_empty() {
            unsafe { Retained::retain(top as *const NSMenu as *mut NSMenu) }
        } else {
            item_at(top, path).and_then(|i| i.submenu())
        };
        // Open from the run loop (like a real click), not inside the caller, so the
        // app's event loop keeps running while the menu is open.
        let Some(m) = menu else { return };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        PENDING_POPUP.with(|p| *p.borrow_mut() = Some((m, x, y)));
        let popper: Retained<Popper> = unsafe { msg_send![Popper::alloc(mtm), init] };
        unsafe {
            let _: () = msg_send![&*popper, performSelector: sel!(pop), withObject: Option::<&AnyObject>::None, afterDelay: 0.0f64];
        }
        POPPER.with(|p| *p.borrow_mut() = Some(popper));
    }

    /// Draw a card offscreen to PNG (development preview, no window needed).
    pub fn render_png(card: &Card, dark: bool) -> Option<Vec<u8>> {
        use objc2_app_kit::{
            NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua,
            NSAppearanceNameDarkAqua, NSBitmapImageFileType,
        };
        let mtm = MainThreadMarker::new()?;
        let view = build(mtm, card);
        let name = unsafe {
            if dark {
                NSAppearanceNameDarkAqua
            } else {
                NSAppearanceNameAqua
            }
        };
        let appearance = NSAppearance::appearanceNamed(name)?;
        view.setAppearance(Some(&appearance));
        let bounds = view.bounds();
        let rep = view.bitmapImageRepForCachingDisplayInRect(bounds)?;
        view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        let data = unsafe {
            rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
        }?;
        Some(data.to_vec())
    }
}
