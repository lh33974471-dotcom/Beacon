use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut run = false;
    args.retain(|a| {
        if a == "--run" {
            run = true;
            false
        } else {
            true
        }
    });
    if args.is_empty() {
        eprintln!("usage: wireui-tis [--run] <file.tis> [more files...]");
        return ExitCode::from(2);
    }
    let mut failed = 0usize;
    if run {
        let mut interp = wireui::script::interp::Interp::new();
        let _log = wireui::script::testhost::install_test_host(&mut interp);
        for path in &args {
            match interp.run_file(std::path::Path::new(path)) {
                Ok(()) => println!("{}: RUN OK", path),
                Err(e) => {
                    eprintln!("{}: RUN ERROR: {}", path, e.0);
                    failed += 1;
                }
            }
        }
        for line in &interp.output {
            println!("  stdout: {}", line);
        }
        return if failed == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    for path in &args {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{}: read error: {}", path, e);
                failed += 1;
                continue;
            }
        };
        match wireui::script::parser::parse(&source) {
            Ok(stmts) => println!("{}: OK ({} top-level statements)", path, stmts.len()),
            Err(e) => {
                eprintln!("{}: PARSE ERROR at {}", path, e);
                failed += 1;
            }
        }
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
