use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut a_path = None;
    let mut b_path = None;
    let mut crop_a_top = 0u32;
    let mut heatmap = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--crop-a-top" => {
                crop_a_top = args.next().and_then(|s| s.parse().ok()).unwrap_or(0)
            }
            "--heatmap" => heatmap = args.next(),
            other => {
                if a_path.is_none() {
                    a_path = Some(other.to_string());
                } else {
                    b_path = Some(other.to_string());
                }
            }
        }
    }
    let (a_path, b_path) = match (a_path, b_path) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            eprintln!("usage: wireui-diff <a.png> <b.png> [--crop-a-top N] [--heatmap out.png]");
            return ExitCode::from(2);
        }
    };
    let a = image::open(&a_path).expect("open a").to_rgba8();
    let b = image::open(&b_path).expect("open b").to_rgba8();
    let a = image::imageops::crop_imm(&a, 0, crop_a_top, a.width(), a.height() - crop_a_top)
        .to_image();
    let w = a.width().min(b.width());
    let h = a.height().min(b.height());
    let mut sum: u64 = 0;
    let mut over: u64 = 0;
    let mut heat = image::RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let pa = a.get_pixel(x, y);
            let pb = b.get_pixel(x, y);
            let d = pa
                .0
                .iter()
                .take(3)
                .zip(pb.0.iter().take(3))
                .map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs())
                .max()
                .unwrap_or(0);
            sum += d as u64;
            if d > 24 {
                over += 1;
                heat.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            } else {
                let g = pa.0[0] / 2 + 96;
                heat.put_pixel(x, y, image::Rgba([g, g, g, 255]));
            }
        }
    }
    let total = (w * h) as u64;
    let mean = sum as f64 / total as f64;
    let pct = over as f64 * 100.0 / total as f64;
    println!(
        "{} vs {}: {}x{} mean-abs-diff {:.2}/255, {:.2}% pixels differ >24",
        a_path, b_path, w, h, mean, pct
    );
    if let Some(hm) = heatmap {
        heat.save(&hm).ok();
        println!("heatmap: {}", hm);
    }
    ExitCode::SUCCESS
}
