use std::collections::HashSet;

use resvg::{tiny_skia, usvg};

use super::*;

fn parse(svg: &str) -> usvg::Tree {
    usvg::Tree::from_str(svg, &usvg::Options::default()).unwrap_or_else(|e| panic!("{e}: {svg}"))
}

#[test]
fn all_icons_parse() {
    for &icon in Icon::ALL {
        for mono in [false, true] {
            let tree = parse(icon.svg(mono));
            assert_eq!(tree.size().width(), 16.0, "{}", icon.name());
            assert_eq!(tree.size().height(), 16.0, "{}", icon.name());
            assert!(icon.svg(mono).contains(r#"viewBox="0 0 16 16""#));
            assert!(
                !icon.svg(mono).contains('$'),
                "unreplaced color token in {}",
                icon.name()
            );
        }
    }
}

#[test]
fn names_unique_and_indexed() {
    let mut names = HashSet::new();
    for (i, &icon) in Icon::ALL.iter().enumerate() {
        assert_eq!(icon as usize, i);
        assert!(
            names.insert(icon.name()),
            "duplicate icon name {}",
            icon.name()
        );
        assert!(
            icon.name()
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-')
        );
    }
}

#[test]
fn mono_variant_is_white() {
    for &icon in Icon::ALL {
        let svg = icon.svg(true);
        for hex in [palette::BLUE, palette::RED, palette::GREEN, palette::PURPLE] {
            assert!(
                !svg.contains(hex),
                "{} mono variant contains {hex}",
                icon.name()
            );
        }
    }
}

#[test]
fn every_icon_draws_something() {
    for &icon in Icon::ALL {
        let pixmap = render(icon, false, 2.0);
        let painted = pixmap.pixels().iter().filter(|p| p.alpha() > 0).count();
        assert!(
            painted > 20,
            "{} renders nearly empty ({painted} px)",
            icon.name()
        );
    }
}

#[test]
fn object_types() {
    assert_eq!(
        Icon::for_object_type("PACKAGE BODY"),
        Some(Icon::PackageBody)
    );
    assert_eq!(Icon::for_object_type("table"), Some(Icon::Table));
    assert_eq!(Icon::for_object_type("NOPE"), None);
}

fn render(icon: Icon, mono: bool, scale: f32) -> tiny_skia::Pixmap {
    let tree = parse(icon.svg(mono));
    let size = (16.0 * scale) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("pixmap");
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap
}

/// Writes a contact sheet of all icons to `$ORUI_ICON_PREVIEW` (a PNG path):
/// dark and light backgrounds, color and mono (text-tinted) variants, at 1x,
/// 2x and an 8x zoom for inspection.
#[test]
fn preview_sheet() {
    let Ok(path) = std::env::var("ORUI_ICON_PREVIEW") else {
        return;
    };
    let scales = [1.0f32, 2.0, 6.0];
    let cell = |s: f32| (16.0 * s + 8.0 * s.min(2.0)) as u32;
    let cols = 12u32;
    let rows = (Icon::ALL.len() as u32).div_ceil(cols);
    // Panels: for each background (dark, light) × variant (color, mono) × scale.
    let backgrounds = [
        (0x1e, 0x1f, 0x22, [0xdf, 0xe1, 0xe5]),
        (0xff, 0xff, 0xff, [0x1f, 0x23, 0x28]),
    ];
    let panel_w: u32 = scales.iter().map(|&s| cell(s) * cols + 16).sum();
    let panel_h: u32 = scales.iter().map(|&s| cell(s) * rows).max().unwrap() + 16;
    let mut sheet = tiny_skia::Pixmap::new(panel_w, panel_h * 4).expect("sheet");
    let mut y0 = 0u32;
    for (r, g, b, text) in backgrounds {
        for mono in [false, true] {
            let mut bg = tiny_skia::Paint::default();
            bg.set_color_rgba8(r, g, b, 255);
            sheet.fill_rect(
                tiny_skia::Rect::from_xywh(0.0, y0 as f32, panel_w as f32, panel_h as f32).unwrap(),
                &bg,
                tiny_skia::Transform::identity(),
                None,
            );
            let mut x0 = 8u32;
            for &scale in &scales {
                for (i, &icon) in Icon::ALL.iter().enumerate() {
                    let use_mono = mono || !icon.is_colored();
                    let mut pm = render(icon, use_mono, scale);
                    if use_mono {
                        // Tint white to the text color like egui does.
                        for p in pm.pixels_mut() {
                            let a = p.alpha();
                            let c = |v: u8| ((v as u16 * a as u16) / 255) as u8;
                            *p = tiny_skia::PremultipliedColorU8::from_rgba(
                                c(text[0]),
                                c(text[1]),
                                c(text[2]),
                                a,
                            )
                            .unwrap();
                        }
                    }
                    let col = i as u32 % cols;
                    let row = i as u32 / cols;
                    let pad = (cell(scale) as f32 - 16.0 * scale) / 2.0;
                    sheet.draw_pixmap(
                        (x0 + col * cell(scale)) as i32 + pad as i32,
                        (y0 + 8 + row * cell(scale)) as i32 + pad as i32,
                        pm.as_ref(),
                        &tiny_skia::PixmapPaint::default(),
                        tiny_skia::Transform::identity(),
                        None,
                    );
                }
                x0 += cell(scale) * cols + 16;
            }
            y0 += panel_h;
        }
    }
    sheet.save_png(&path).expect("writing preview");
    pixel_sheet(&path.replace(".png", "-pixels.png"));
    for (i, icon) in Icon::ALL.iter().enumerate() {
        println!(
            "{:>2} r{} c{:>2} {}",
            i,
            i / cols as usize,
            i % cols as usize,
            icon.name()
        );
    }
}

/// 1x and 2x renders magnified with nearest-neighbor, to judge pixel fit.
fn pixel_sheet(path: &str) {
    let zoom = 6u32;
    let cols = 12u32;
    let rows = (Icon::ALL.len() as u32).div_ceil(cols);
    let cell = |s: u32| 16 * s * zoom + 4 * zoom;
    let width = (cell(1) + cell(2)) * cols + 8;
    let half_h = cell(2) * rows + 8;
    let mut out = tiny_skia::Pixmap::new(width, half_h * 2).expect("sheet");
    let themes = [
        ((0x1e, 0x1f, 0x22), [0xdf, 0xe1, 0xe5]),
        ((0xff, 0xff, 0xff), [0x1f, 0x23, 0x28]),
    ];
    for (half, (bg, text)) in themes.into_iter().enumerate() {
        let y0 = half as u32 * half_h;
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(bg.0, bg.1, bg.2, 255);
        let rect = tiny_skia::Rect::from_xywh(0.0, y0 as f32, width as f32, half_h as f32).unwrap();
        out.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);
        let mut x0 = 4;
        for scale in [1u32, 2] {
            for (i, &icon) in Icon::ALL.iter().enumerate() {
                let mono = !icon.is_colored();
                let pm = render(icon, mono, scale as f32);
                let ox = x0 + (i as u32 % cols) * cell(scale);
                let oy = y0 + 4 + (i as u32 / cols) * cell(2);
                for (j, p) in pm.pixels().iter().enumerate() {
                    let a = p.alpha();
                    if a == 0 {
                        continue;
                    }
                    let c = p.demultiply();
                    let (r, g, b) = if mono {
                        (text[0], text[1], text[2])
                    } else {
                        (c.red(), c.green(), c.blue())
                    };
                    let mut paint = tiny_skia::Paint::default();
                    paint.set_color_rgba8(r, g, b, a);
                    let px = (j as u32 % (16 * scale)) * zoom + ox;
                    let py = (j as u32 / (16 * scale)) * zoom + oy;
                    let rect =
                        tiny_skia::Rect::from_xywh(px as f32, py as f32, zoom as f32, zoom as f32)
                            .unwrap();
                    out.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);
                }
            }
            x0 += cell(scale) * cols;
        }
    }
    out.save_png(path).expect("writing pixel preview");
}
