//! The GPU foundation: the context, the arena, shapes, curves, read-back, PNG.

use super::*;
use crate::arena::{FrameArena, ShapeBatch};
use crate::gpu::{CurveUniforms, Globals};
use crate::target::{decode_png, encode_png, OffscreenTarget};

/// Draws with the foundation into a 200 x 120 point target at 2x.
fn draw(
    ctx: &RenderContext,
    body: impl FnOnce(&mut wgpu::RenderPass<'_>, &mut FrameArena, &mut ShapeBatch),
) -> Vec<u8> {
    let target = OffscreenTarget::new(ctx, 400, 240);
    let mut arena = FrameArena::new(ctx);
    let mut batch = ShapeBatch::new();
    arena.begin_frame();
    batch.begin_frame(&mut arena, 2.0);
    let mut encoder = ctx.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("test"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        body(&mut pass, &mut arena, &mut batch);
    }
    arena.upload(
        &ctx.queue,
        &Globals {
            view_size: [200.0, 120.0],
            scale: 2.0,
            time: 0.0,
        },
    );
    ctx.queue.submit([encoder.finish()]);
    target.read_rgba(ctx).unwrap()
}

#[test]
fn the_context_names_its_adapter() {
    let Some(ctx) = gpu() else { return };
    println!("adapter: {}", ctx.adapter_info);
    assert!(!ctx.adapter_info.is_empty());
}

#[test]
fn shapes_are_drawn_where_they_are_put() {
    let Some(ctx) = gpu() else { return };
    let rgba = draw(&ctx, |pass, arena, batch| {
        batch.rect(arena, [10.0, 10.0, 50.0, 30.0], [1.0, 0.0, 0.0, 1.0]);
        batch.rounded_rect(arena, [100.0, 10.0, 60.0, 60.0], [0.0, 1.0, 0.0, 1.0], 30.0);
        batch.rect(arena, [20.0, 80.0, 100.0, 20.0], [0.0, 0.0, 1.0, 0.5]);
        batch.hgradient(
            arena,
            [130.0, 90.0, 60.0, 20.0],
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            0.0,
        );
        batch.vline(arena, 190.0, 0.0, 120.0, [1.0, 1.0, 0.0, 1.0], 1.0);
        batch.flush(pass, arena, &ctx.shape_over, None);
        assert_eq!(batch.count(), 5);
    });
    std::fs::write(review_path("foundation-shapes.png"), encode_png(400, 240, &rgba)).unwrap();
    // Points are doubled to pixels.
    assert_color(pixel(&rgba, 400, 70, 50), [255, 0, 0], 1, "inside the red rectangle");
    assert_color(pixel(&rgba, 400, 10, 50), [0, 0, 0], 1, "left of the red rectangle");
    assert_color(
        pixel(&rgba, 400, 260, 80),
        [0, 255, 0],
        1,
        "the middle of the green circle",
    );
    assert_color(
        pixel(&rgba, 400, 204, 24),
        [0, 0, 0],
        1,
        "the corner of its box is outside the circle",
    );
    assert_color(pixel(&rgba, 400, 140, 180), [0, 0, 128], 1, "half blue over black");
    let (dark, light) = (pixel(&rgba, 400, 270, 200), pixel(&rgba, 400, 370, 200));
    assert!(
        dark[0] < 40 && light[0] > 215,
        "the gradient runs left to right: {dark:?} {light:?}"
    );
    // The 1-pixel line is on one pixel column, at full strength.
    assert_color(pixel(&rgba, 400, 380, 4), [255, 255, 0], 1, "the yellow line");
    assert_color(pixel(&rgba, 400, 378, 4), [0, 0, 0], 1, "next to the yellow line");
}

#[test]
fn a_curve_is_a_line_of_even_width_with_a_fill_under_it() {
    let Some(ctx) = gpu() else { return };
    let rgba = draw(&ctx, |pass, arena, _| {
        // A ramp from the bottom left to the top right of the plot, then a spike.
        let mut values: Vec<f32> = (0..200).map(|i| i as f32 / 199.0).collect();
        values[100] = 1.0;
        let slice = arena.push_floats(&values).unwrap();
        let rect = [0.0, 10.0, 200.0, 100.0];
        let fill = CurveUniforms {
            rect,
            color: [0.0, 0.5, 1.0, 1.0],
            count: 200.0,
            pad0: 1.0,
            pad2: 1.0,
            ..Default::default()
        };
        let line = CurveUniforms {
            rect,
            color: [1.0, 1.0, 1.0, 1.0],
            half_width: 1.0,
            count: 200.0,
            pad0: 4.0,
            ..Default::default()
        };
        pass.set_bind_group(0, &arena.globals_group, &[]);
        pass.set_bind_group(2, &ctx.white_lut.bind_group, &[]);
        for (pipeline, uniforms) in [(&ctx.curve_fill, fill), (&ctx.curve_line_over, line)] {
            let offset = arena.push_uniform(&uniforms).unwrap();
            pass.set_pipeline(pipeline);
            pass.set_bind_group(1, &arena.curve_group, &[offset, slice.offset, slice.offset]);
            pass.draw(0..400, 0..1);
        }
    });
    std::fs::write(review_path("foundation-curve.png"), encode_png(400, 240, &rgba)).unwrap();
    // At x = 50 pt the ramp is at a quarter of the plot: y = 10 + 100 * 0.75 = 85 pt.
    assert_color(pixel(&rgba, 400, 100, 170), [255, 255, 255], 2, "on the line");
    assert_color(pixel(&rgba, 400, 100, 150), [0, 0, 0], 1, "over the line");
    let under = pixel(&rgba, 400, 100, 200);
    assert!(
        under[2] > 20 && under[0] < under[2],
        "the fill under the line is blue: {under:?}"
    );
    // The spike reaches the top of the plot.
    assert_color(pixel(&rgba, 400, 201, 24), [255, 255, 255], 60, "the spike");
}

#[test]
fn png_round_trip_keeps_the_pixels() {
    let rgba: Vec<u8> = (0..4 * 3 * 2)
        .map(|i| if i % 4 == 3 { 255 } else { (i * 9) as u8 })
        .collect();
    let (width, height, back) = decode_png(&encode_png(3, 2, &rgba)).unwrap();
    assert_eq!((width, height), (3, 2));
    assert_eq!(back, rgba);
}
