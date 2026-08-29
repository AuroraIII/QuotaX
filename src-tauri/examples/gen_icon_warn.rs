//! 重新生成 icons/icon-warn.ico（超量提醒托盘图标）：
//! 基于主图标 icons/icon.png 各尺寸缩放 + 右下角橙色角标（白描边 + #fb923c 橙点），
//! 输出 PNG-in-ICO 多尺寸格式（poller::warn_tray_icon 用 ico crate 解码，取 ≤48px 最大尺寸）。
//!
//! 用法（在 src-tauri 目录下，替换主图标后运行一次）：
//!   cargo run --example gen_icon_warn

use std::fs::File;

use ico::{IconDir, IconDirEntry, IconImage, ResourceType};

const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
const ORANGE: [u8; 4] = [251, 146, 60, 255]; // #fb923c
const WHITE: [u8; 4] = [255, 255, 255, 255];

fn main() -> Result<(), std::io::Error> {
    let src = IconImage::read_png(File::open("icons/icon.png")?)?;

    let mut dir = IconDir::new(ResourceType::Icon);
    for &s in &SIZES {
        let mut rgba = resize_box(&src, s);
        draw_badge(&mut rgba, s);
        let img = IconImage::from_rgba_data(s, s, rgba);
        let entry = IconDirEntry::encode_as_png(&img)?;
        dir.add_entry(entry);
    }

    dir.write(File::create("icons/icon-warn.ico")?)?;
    println!(
        "icon-warn.ico written: {} bytes (sizes {:?})",
        std::fs::metadata("icons/icon-warn.ico")?.len(),
        SIZES
    );
    Ok(())
}

/// box-filter（平均池化）缩放到 s×s，对大图缩小的颜色保真最好。
fn resize_box(src: &IconImage, s: u32) -> Vec<u8> {
    let (w, h) = (src.width() as usize, src.height() as usize);
    let data = src.rgba_data();
    let mut out = vec![0u8; (s as usize) * (s as usize) * 4];
    for dy in 0..s as usize {
        for dx in 0..s as usize {
            // 目标像素映射回源图的覆盖块
            let x0 = dx * w / s as usize;
            let x1 = ((dx + 1) * w / s as usize).max(x0 + 1);
            let y0 = dy * h / s as usize;
            let y1 = ((dy + 1) * h / s as usize).max(y0 + 1);
            let (mut r, mut g, mut b, mut a, mut n) = (0usize, 0usize, 0usize, 0usize, 0usize);
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    let p = &data[(y * w + x) * 4..(y * w + x) * 4 + 4];
                    r += p[0] as usize * p[3] as usize; // 预乘 alpha 再平均
                    g += p[1] as usize * p[3] as usize;
                    b += p[2] as usize * p[3] as usize;
                    a += p[3] as usize;
                    n += 1;
                }
            }
            let o = (dy * s as usize + dx) * 4;
            if a == 0 {
                out[o..o + 4].copy_from_slice(&[0, 0, 0, 0]);
            } else {
                out[o] = (r / a) as u8;
                out[o + 1] = (g / a) as u8;
                out[o + 2] = (b / a) as u8;
                out[o + 3] = (a / n) as u8;
            }
        }
    }
    out
}

/// 右下角橙色角标：橙实心圆（直径 38%）+ 白色描边（宽 max(1, 7.5%)），
/// 边缘按像素覆盖率做 1px 抗锯齿混合。
fn draw_badge(rgba: &mut [u8], s: u32) {
    let s = s as f32;
    let d = s * 0.38; // 橙点直径
    let m = s * 0.07; // 距边留白
    let stroke = (s * 0.075).max(1.0);
    let cx = s - m - d / 2.0;
    let cy = s - m - d / 2.0;
    let r = d / 2.0;
    let outer = r + stroke / 2.0; // 白环外半径（描边以橙圆边为中心）

    let n = s as usize;
    for y in 0..n {
        for x in 0..n {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            // 覆盖率：圆边缘 1px 内线性过渡
            let cover_orange = (r - dist + 0.5).clamp(0.0, 1.0);
            let cover_ring = (outer - dist + 0.5).clamp(0.0, 1.0);
            if cover_ring <= 0.0 {
                continue;
            }
            let o = (y * n + x) * 4;
            let base: [u8; 4] = [rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3]];
            let with_ring = blend(base, WHITE, cover_ring);
            let with_dot = blend(with_ring, ORANGE, cover_orange);
            rgba[o..o + 4].copy_from_slice(&with_dot);
        }
    }
}

fn blend(base: [u8; 4], top: [u8; 4], alpha: f32) -> [u8; 4] {
    let a = alpha * top[3] as f32 / 255.0;
    let mix = |b: u8, t: u8| (b as f32 * (1.0 - a) + t as f32 * a).round() as u8;
    [
        mix(base[0], top[0]),
        mix(base[1], top[1]),
        mix(base[2], top[2]),
        255,
    ]
}
