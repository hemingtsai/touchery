//! Renders the touchery lightning-bolt icon as PNGs for the .iconset.
//!
//! Usage: cargo run --release --example gen_icon -- <output-dir>

use std::path::PathBuf;

/// Normalized polygon points (y down): classic lightning bolt.
const BOLT: [(f32, f32); 7] = [
    (0.60, 0.02),
    (0.20, 0.55),
    (0.45, 0.55),
    (0.35, 0.98),
    (0.80, 0.42),
    (0.52, 0.42),
    (0.68, 0.02),
];

fn inside(px: f32, py: f32) -> bool {
    let mut inside = false;
    let mut j = BOLT.len() - 1;
    for i in 0..BOLT.len() {
        let (xi, yi) = BOLT[i];
        let (xj, yj) = BOLT[j];
        if ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn render(size: u32) -> image::RgbaImage {
    // Rounded-rect badge background + white bolt, macOS app-icon style.
    let mut img = image::RgbaImage::new(size, size);

    // Supersample for smooth edges.
    let ss = 4u32;
    let total = size * ss;

    let radius = (total as f32) * 0.225; // macOS squircle-ish rounding
    let in_rounded = |x: f32, y: f32| -> bool {
        let cx = (total as f32 - 1.0) / 2.0;
        let dx = (x - cx).abs();
        let dy = (y - cx).abs();
        let half = cx;
        if dx <= half - radius || dy <= half - radius {
            dx <= half && dy <= half
        } else {
            let qx = dx - (half - radius);
            let qy = dy - (half - radius);
            qx * qx + qy * qy <= radius * radius
        }
    };

    // Gradient-ish two-tone dark background with a subtle vertical ramp.
    for y in 0..total {
        for x in 0..total {
            if !in_rounded(x as f32 + 0.5, y as f32 + 0.5) {
                continue;
            }
            let t = y as f32 / total as f32;
            let (r, g, b) = (
                (0.16 + 0.10 * t) * 255.0,
                (0.16 + 0.10 * t) * 255.0,
                (0.20 + 0.12 * t) * 255.0,
            );
            img.put_pixel(
                x / ss,
                y / ss,
                image::Rgba([r as u8, g as u8, b as u8, 255]),
            );
        }
    }

    // White bolt, supersampled coverage blend.
    for y in 0..total {
        for x in 0..total {
            if inside(
                (x as f32 + 0.5) / total as f32,
                (y as f32 + 0.5) / total as f32,
            ) && in_rounded(x as f32 + 0.5, y as f32 + 0.5)
            {
                img.put_pixel(x / ss, y / ss, image::Rgba([245, 245, 250, 255]));
            }
        }
    }

    img
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/iconset"));

    std::fs::create_dir_all(&out).expect("create icon output dir");

    // .iconset naming convention -> pixel size
    const VARIANTS: [(&str, u32); 10] = [
        ("icon_16x16.png", 16),
        ("icon_16x16@2x.png", 32),
        ("icon_32x32.png", 32),
        ("icon_32x32@2x.png", 64),
        ("icon_128x128.png", 128),
        ("icon_128x128@2x.png", 256),
        ("icon_256x256.png", 256),
        ("icon_256x256@2x.png", 512),
        ("icon_512x512.png", 512),
        ("icon_512x512@2x.png", 1024),
    ];

    for (name, size) in VARIANTS {
        let img = render(size);
        let path = out.join(name);
        img.save(&path).unwrap_or_else(|e| panic!("save {path:?}: {e}"));
        println!("wrote {}", path.display());
    }
}
