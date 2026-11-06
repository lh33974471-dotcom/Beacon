use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut page = None;
    let mut eval = None;
    let mut size = (800u32, 600u32);
    let mut scale = 1.0f32;
    let mut pump_ms = 200.0f64;
    let mut out = PathBuf::from("out.png");
    let mut base: Option<PathBuf> = None;
    let mut platform = "OSX".to_string();
    let mut archive: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--page" => page = args.next().map(PathBuf::from),
            "--eval" => eval = args.next(),
            "--size" => {
                if let Some(s) = args.next() {
                    let parts: Vec<u32> =
                        s.split('x').filter_map(|p| p.parse().ok()).collect();
                    if parts.len() == 2 {
                        size = (parts[0], parts[1]);
                    }
                }
            }
            "--scale" => scale = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.0),
            "--pump-ms" => {
                pump_ms = args.next().and_then(|s| s.parse().ok()).unwrap_or(200.0)
            }
            "--out" => out = args.next().map(PathBuf::from).unwrap_or(out),
            "--base" => base = args.next().map(PathBuf::from),
            "--platform" => platform = args.next().unwrap_or(platform),
            "--archive" => archive = args.next().map(PathBuf::from),
            other => {
                eprintln!("unknown argument: {}", other);
                return ExitCode::from(2);
            }
        }
    }
    let page = match page {
        Some(p) => p,
        None => {
            eprintln!("usage: wireui-render --page <file.html|name-in-archive> [--archive resources.rc] [--eval code] [--size WxH] [--scale N] [--pump-ms M] [--base dir] [--out out.png]");
            return ExitCode::from(2);
        }
    };

    let loaded = match &archive {
        Some(rc) => {
            let bytes = match std::fs::read(rc) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("read archive {}: {}", rc.display(), e);
                    return ExitCode::FAILURE;
                }
            };
            match wireui::engine::archive::Archive::parse(&bytes) {
                Ok(a) => wireui::engine::host::load_page_archive(
                    a,
                    &page.to_string_lossy(),
                    &platform,
                    None,
                ),
                Err(e) => {
                    eprintln!("parse archive: {}", e);
                    return ExitCode::FAILURE;
                }
            }
        }
        None => wireui::engine::host::load_page(&page, base, &platform),
    };
    let loaded = match loaded {
        Ok(l) => l,
        Err(e) => {
            eprintln!("load failed: {}", e);
            return ExitCode::FAILURE;
        }
    };
    let mut interp = loaded.interp;
    let engine = loaded.engine;
    engine.borrow_mut().scale = scale;

    if let Err(e) = wireui::engine::host::pump_timers(&mut interp, &engine, 5.0) {
        eprintln!("initial timer pump failed: {}", e.0);
        return ExitCode::FAILURE;
    }

    if let Some(code) = eval {
        if let Err(e) = interp.run_source(&code) {
            eprintln!("eval failed: {}", e.0);
            return ExitCode::FAILURE;
        }
    }

    if let Err(e) = wireui::engine::host::pump_timers(&mut interp, &engine, pump_ms) {
        eprintln!("timer pump failed: {}", e.0);
        return ExitCode::FAILURE;
    }
    wireui::engine::host::focus_first_input(&engine);
    for line in interp.output.drain(..) {
        println!("[script] {}", line);
    }

    let e = engine.borrow();
    let styles = wireui::engine::style::compute_styles(&e.doc, &e.sheets);
    // WIREUI_NO_SYSTEM_FONTS=1 reproduces the Win7/8 path (embedded fonts only).
    let system_fonts = std::env::var("WIREUI_NO_SYSTEM_FONTS").map_or(true, |v| v != "1");
    let mut text_system = wireui::engine::layout::TextSystem::new_with(system_fonts);
    for sheet in &e.sheets {
        for (family, data) in &sheet.font_faces {
            text_system.register_font(family, data.clone());
        }
    }
    let layout = wireui::engine::layout::layout_document(
        &e.doc,
        &styles,
        &mut text_system,
        (size.0 as f32, size.1 as f32),
        scale,
    );
    drop(e);
    engine.borrow_mut().last_rects = layout.rects.clone();
    wireui::engine::host::record_fg_overlays(&mut interp, &engine);
    let e = engine.borrow();
    let pseudo = wireui::engine::style::compute_pseudo_boxes(&e.doc, &e.sheets, &styles);
    let scene = wireui::engine::paint::paint_document_overlaid(
        &e.doc,
        &styles,
        &layout,
        scale,
        &std::collections::HashMap::new(),
        &pseudo,
        e.now_ms,
        &e.content_overlays,
    );
    let width = (size.0 as f32 * scale) as u32;
    let height = (size.1 as f32 * scale) as u32;
    match wireui::engine::paint::render_to_png(&scene, width, height, &out) {
        Ok(()) => {
            println!(
                "rendered {} nodes to {} ({}x{})",
                e.doc.arena.len(),
                out.display(),
                width,
                height
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("render failed: {}", err);
            ExitCode::FAILURE
        }
    }
}
