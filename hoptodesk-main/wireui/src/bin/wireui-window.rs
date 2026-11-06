use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut page = None;
    let mut base: Option<PathBuf> = None;
    let mut size = (800u32, 600u32);
    let mut platform = "OSX".to_string();
    let mut title = "wireui".to_string();
    let mut eval: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--page" => page = args.next().map(PathBuf::from),
            "--base" => base = args.next().map(PathBuf::from),
            "--platform" => platform = args.next().unwrap_or(platform),
            "--title" => title = args.next().unwrap_or(title),
            "--eval" => eval = args.next(),
            "--size" => {
                if let Some(s) = args.next() {
                    let p: Vec<u32> = s.split('x').filter_map(|x| x.parse().ok()).collect();
                    if p.len() == 2 { size = (p[0], p[1]); }
                }
            }
            other => { eprintln!("unknown arg {}", other); return ExitCode::from(2); }
        }
    }
    let page = match page { Some(p) => p, None => { eprintln!("usage: wireui-window --page <file.html> [--base dir] [--size WxH] [--eval code]"); return ExitCode::from(2); } };
    match wireui::engine::window::run_window(&page, base, &platform, size, &title, eval.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("window error: {}", e); ExitCode::FAILURE }
    }
}
