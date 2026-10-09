//! The text layer: fonts, layout, the recorder, the repaint of only what
//! changed, and the picture on the GPU.

use super::*;
use crate::arena::FrameArena;
use crate::gpu::Globals;
use crate::palette::{with_alpha, Palette, Theme};
use crate::target::{encode_png, OffscreenTarget};
use crate::text::fonts::{self, Weight};
use crate::text::{HAlign, OverlayContext, Rect, TextLayer, VAlign};

/// False (with a note) when the machine has none of the system fonts.
fn have_fonts() -> bool {
    if !fonts::available() {
        println!("no system font: skipped");
    }
    fonts::available()
}

#[test]
fn digits_have_one_width_and_text_has_a_size() {
    if !have_fonts() {
        return;
    }
    for font in [fonts::ui(12.0, Weight::Regular), fonts::mono(11.0, Weight::Regular)] {
        let ones = font.measure("111.1", 0.0);
        let eights = font.measure("888.8", 0.0);
        assert!((ones - eights).abs() < 1e-4, "{ones} vs {eights}");
        assert!(font.measure("Spectrum", 0.0) > font.measure("Spec", 0.0));
        assert!(font.ascent() > font.cap_height() * 0.9);
        assert!(font.cap_height() > font.size * 0.5 && font.cap_height() < font.size * 0.9);
        assert!(font.descent() > 0.0 && font.descent() < font.size * 0.5);
    }
    // Inside one optical size the width follows the point size. Tracking adds room after every glyph.
    let (small, large) = (fonts::ui(10.0, Weight::Regular), fonts::ui(16.0, Weight::Regular));
    let ratio = large.measure("Loudness", 0.0) / small.measure("Loudness", 0.0);
    assert!((ratio - 1.6).abs() < 0.01, "{ratio}");
    // Large numerals take the display cut where the font has one: never wider than the text cut scaled up.
    let display =
        fonts::ui(30.0, Weight::Light).measure("-13.8", 0.0) / fonts::ui(10.0, Weight::Light).measure("-13.8", 0.0);
    assert!(display > 2.7 && display < 3.001, "{display}");
    let tracked = small.measure("LUFS", 1.5) - small.measure("LUFS", 0.0);
    assert!((tracked - 6.0).abs() < 1e-3, "{tracked}");
    // The signs and the note sign the panels use have a glyph in some font.
    let sharp = small.layout("C\u{266F}4 \u{2212}3 \u{00B1}0 \u{00A2}", 0.0);
    assert_eq!(sharp.glyphs.len(), 11, "every character has a glyph");
    // A bolder weight is another face.
    assert_ne!(fonts::ui(12.0, Weight::Bold), fonts::ui(12.0, Weight::Regular));
    assert_eq!(fonts::ui(12.0, Weight::Bold), fonts::ui(12.0, Weight::Bold));
}

#[test]
fn alignment_puts_the_ink_box_where_it_is_asked() {
    if !have_fonts() {
        return;
    }
    let font = fonts::ui(12.0, Weight::Regular);
    let mut o = OverlayContext::new(300.0, 100.0, 2.0);
    let white = [1.0; 4];
    let width = o.text("Peak", 150.0, 50.0, &font, white, HAlign::Center, VAlign::Middle);
    assert!((width - font.measure("Peak", 0.0)).abs() < 1e-4);
    let (text, ink) = &o.labels()[0];
    assert_eq!(text, "Peak");
    assert!((ink.mid_x() - 150.0).abs() < 0.01, "centred in x: {ink:?}");
    // The ink box is the cap height plus a little, around the middle.
    assert!((ink.mid_y() - 50.0).abs() < 1.0, "centred in y: {ink:?}");
    assert!((ink.h - (font.cap_height() + 2.5)).abs() < 1e-4);

    o.text("R", 300.0, 0.0, &font, white, HAlign::Right, VAlign::Top);
    let ink = o.labels()[1].1;
    assert!(
        (ink.max_x() - 300.0).abs() < 0.01 && (ink.min_y() + 1.0).abs() < 0.01,
        "{ink:?}"
    );
    assert_eq!(
        o.text_bounds("R", 300.0, 0.0, &font, HAlign::Right, VAlign::Top, 0.0),
        ink
    );

    // `is_free` sees the labels drawn so far.
    assert!(!o.is_free(Rect::new(140.0, 45.0, 20.0, 10.0), 2.0, &[]));
    assert!(o.is_free(Rect::new(10.0, 80.0, 20.0, 10.0), 2.0, &[]));
    assert!(!o.is_free(
        Rect::new(10.0, 80.0, 20.0, 10.0),
        2.0,
        &[Rect::new(25.0, 85.0, 5.0, 5.0)]
    ));
    // An empty string draws nothing.
    assert_eq!(o.text("", 0.0, 0.0, &font, white, HAlign::Left, VAlign::Baseline), 0.0);
    assert_eq!(o.items().len(), 2);
    // Every item is on the pixel grid and inside the view.
    for item in o.items() {
        let r = item.rect;
        for v in [r.x, r.y, r.w, r.h] {
            assert_eq!((v * 2.0).fract(), 0.0, "{r:?} is not pixel aligned");
        }
        assert!(r.min_x() >= 0.0 && r.max_x() <= 300.0 && r.min_y() >= 0.0 && r.max_y() <= 100.0);
    }
}

fn readout(o: &mut OverlayContext, value: &str) {
    let label = fonts::ui(10.0, Weight::Medium);
    let number = fonts::ui(15.0, Weight::Regular);
    let dim = [0.7, 0.75, 0.85, 1.0];
    o.text_tracked(
        "INTEGRATED",
        12.0,
        16.0,
        &label,
        dim,
        HAlign::Left,
        VAlign::Baseline,
        0.6,
    );
    o.text(value, 12.0, 42.0, &number, [1.0; 4], HAlign::Left, VAlign::Baseline);
    o.text("LUFS", 80.0, 42.0, &label, dim, HAlign::Left, VAlign::Baseline);
    o.fill_rect(Rect::new(150.0, 10.0, 60.0, 30.0), [0.1, 0.3, 0.6, 0.9], 6.0);
    o.stroke_rect(Rect::new(150.5, 10.5, 59.0, 29.0), [1.0, 1.0, 1.0, 0.5], 6.0, 1.0);
    o.text("Box", 180.0, 25.0, &label, [1.0; 4], HAlign::Center, VAlign::Middle);
}

#[test]
fn a_redraw_paints_only_what_changed() {
    if !have_fonts() {
        return;
    }
    let mut layer = TextLayer::new();
    layer.resize(240.0, 60.0, 2.0);
    let mut first = OverlayContext::new(240.0, 60.0, 2.0);
    readout(&mut first, "\u{2212}14.2");
    layer.redraw(&first);
    assert_eq!(layer.last_item_count, 6);
    assert_eq!(layer.last_painted_count, 6, "the first redraw paints all");
    // The text inside the box needs no rectangle of its own for the GPU pass.
    assert_eq!(layer.rects().len(), 5, "{:?}", layer.rects());

    // The same picture again: nothing to paint.
    let mut same = OverlayContext::new(240.0, 60.0, 2.0);
    readout(&mut same, "\u{2212}14.2");
    layer.redraw(&same);
    assert_eq!(layer.last_painted_count, 0);

    // The number changes: only the number is painted, and "LUFS" next to it stays.
    let mut changed = OverlayContext::new(240.0, 60.0, 2.0);
    readout(&mut changed, "\u{2212}13.9");
    layer.redraw(&changed);
    assert_eq!(layer.last_painted_count, 1, "{:?}", layer.last_painted_rects);
    assert_eq!(layer.redraw_count, 3);
    assert_eq!(layer.last_labels.len(), 4);

    // The bitmap has ink where the number is, and none in the empty corner.
    let (pixels, width, _) = layer.pixels().unwrap();
    let alpha_in = |r: Rect| -> u32 {
        let mut sum = 0;
        for y in (r.min_y() * 2.0) as u32..(r.max_y() * 2.0) as u32 {
            for x in (r.min_x() * 2.0) as u32..(r.max_x() * 2.0) as u32 {
                sum += pixels[((y * width + x) * 4 + 3) as usize] as u32;
            }
        }
        sum
    };
    assert!(alpha_in(layer.last_labels[1].1) > 2_000, "the number has ink");
    assert_eq!(
        alpha_in(Rect::new(0.0, 44.0, 140.0, 16.0)),
        0,
        "the empty area is clear"
    );

    // An item that goes away is cleared.
    let mut fewer = OverlayContext::new(240.0, 60.0, 2.0);
    let font = fonts::ui(10.0, Weight::Medium);
    fewer.text(
        "LUFS",
        80.0,
        36.0,
        &font,
        [0.7, 0.75, 0.85, 1.0],
        HAlign::Left,
        VAlign::Baseline,
    );
    layer.redraw(&fewer);
    let (pixels, width, height) = layer.pixels().unwrap();
    let mut left_ink = 0u32;
    for y in 0..height {
        for x in 0..150 {
            left_ink += pixels[((y * width + x) * 4 + 3) as usize] as u32;
        }
    }
    assert_eq!(left_ink, 0, "the label and the number are gone");
    assert_eq!(layer.rects().len(), 1);
}

#[test]
fn the_overlay_is_drawn_over_the_panel() {
    let Some(ctx) = gpu() else { return };
    if !have_fonts() {
        return;
    }
    let palette = Palette::new(&Theme::default(), false);
    let (width, height, scale) = (360.0f32, 150.0f32, 2.0f32);
    let mut layer = TextLayer::new();
    layer.resize(width, height, scale);
    let mut o = OverlayContext::new(width, height, scale);
    readout(&mut o, "\u{2212}14.2");
    let mono = fonts::mono(10.0, Weight::Regular);
    for (i, label) in ["20", "50", "100", "200", "500", "1k", "2k", "5k", "10k", "20k"]
        .iter()
        .enumerate()
    {
        o.text(
            label,
            20.0 + i as f32 * 35.0,
            140.0,
            &mono,
            palette.text_dim,
            HAlign::Center,
            VAlign::Baseline,
        );
    }
    for (i, weight) in [
        Weight::Light,
        Weight::Regular,
        Weight::Medium,
        Weight::Semibold,
        Weight::Bold,
    ]
    .into_iter()
    .enumerate()
    {
        let font = fonts::ui(13.0, weight);
        o.text(
            &format!("{weight:?} 0123456789 A\u{266F}3 \u{2212}3.5 dB"),
            12.0,
            62.0 + i as f32 * 15.0,
            &font,
            palette.text,
            HAlign::Left,
            VAlign::Baseline,
        );
    }
    o.line([230.0, 20.0], [350.0, 20.0], palette.warn, 1.0, &[4.0, 3.0]);
    o.line([230.0, 30.0], [350.0, 40.0], palette.accent, 2.0, &[]);
    o.arc([300.0, 90.0], 30.0, 0.0, 4.2, palette.good, 4.0);
    o.arc(
        [300.0, 90.0],
        30.0,
        0.0,
        std::f32::consts::TAU,
        with_alpha(palette.text, 0.15),
        4.0,
    );
    layer.redraw(&o);
    layer.upload(&ctx);

    let target = OffscreenTarget::new(&ctx, (width * scale) as u32, (height * scale) as u32);
    let mut arena = FrameArena::new(&ctx);
    arena.begin_frame();
    let mut encoder = ctx.device.create_command_encoder(&Default::default());
    {
        let bg = palette.background;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("text test"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: bg[0] as f64,
                        g: bg[1] as f64,
                        b: bg[2] as f64,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        layer.draw(&mut pass, &ctx, &mut arena);
    }
    arena.upload(
        &ctx.queue,
        &Globals {
            view_size: [width, height],
            scale,
            time: 0.0,
        },
    );
    ctx.queue.submit([encoder.finish()]);
    let rgba = target.read_rgba(&ctx).unwrap();
    std::fs::write(
        review_path("text-overlay.png"),
        encode_png(target.width, target.height, &rgba),
    )
    .unwrap();

    // Inside the filled box: its blue over the background. Outside every rectangle: the background.
    let inside = pixel(&rgba, target.width, 310, 30);
    assert!(inside[2] > 120 && inside[0] < 60, "the box is blue: {inside:?}");
    assert_color(pixel(&rgba, target.width, 4, 296), [9, 12, 22], 1, "the background");
    // The white number is on the picture.
    let number = layer.last_labels[1].1;
    let mut brightest = 0;
    for y in (number.min_y() * 2.0) as u32..(number.max_y() * 2.0) as u32 {
        for x in (number.min_x() * 2.0) as u32..(number.max_x() * 2.0) as u32 {
            brightest = brightest.max(pixel(&rgba, target.width, x, y)[0]);
        }
    }
    assert!(brightest > 230, "the number is white: {brightest}");
}
