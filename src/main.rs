// Hide the console window on Windows release builds (CLI mode re-attaches below).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cli;
mod converter;
mod gui;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        attach_console();
        return cli::run(args);
    }
    match gui::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Could not start the GUI: {e}");
            ExitCode::FAILURE
        }
    }
}

/// With `windows_subsystem = "windows"` there is no console; reuse the parent's
/// so CLI output still shows up when launched from a terminal.
#[cfg(all(windows, not(debug_assertions)))]
fn attach_console() {
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    unsafe {
        AttachConsole(u32::MAX); // ATTACH_PARENT_PROCESS
    }
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn attach_console() {}
