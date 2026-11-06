use std::io;
use std::process::Command;

use crate::ShutdownResult;

fn run(program: &str, args: &[&str]) -> ShutdownResult {
    let status = Command::new(program).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("{program} exited with status {status}"),
        ))
    }
}

fn unsupported(operation: &str) -> ShutdownResult {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{operation} is not supported on FreeBSD"),
    ))
}

pub fn shutdown() -> ShutdownResult {
    run("shutdown", &["-p", "now"])
}

pub fn force_shutdown() -> ShutdownResult {
    run("halt", &["-p"])
}

pub fn reboot() -> ShutdownResult {
    run("shutdown", &["-r", "now"])
}

pub fn force_reboot() -> ShutdownResult {
    run("reboot", &[])
}

pub fn logout() -> ShutdownResult {
    unsupported("logout")
}

pub fn force_logout() -> ShutdownResult {
    unsupported("force_logout")
}

pub fn sleep() -> ShutdownResult {
    run("acpiconf", &["-s", "3"])
}

pub fn hibernate() -> ShutdownResult {
    unsupported("hibernate")
}
