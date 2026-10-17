//! Command-line mode, compatible with the original C++ tool:
//! `webp-forge <files...> [--max-size KB] [--jobs N]`

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use crate::converter::{self, Config, Event, Level};

struct Colors {
    red: &'static str,
    green: &'static str,
    yellow: &'static str,
    cyan: &'static str,
    bold: &'static str,
    reset: &'static str,
}

fn colors() -> Colors {
    if std::io::stdout().is_terminal() {
        Colors {
            red: "\x1b[0;31m",
            green: "\x1b[0;32m",
            yellow: "\x1b[0;33m",
            cyan: "\x1b[0;36m",
            bold: "\x1b[1m",
            reset: "\x1b[0m",
        }
    } else {
        Colors { red: "", green: "", yellow: "", cyan: "", bold: "", reset: "" }
    }
}

fn usage(program: &str) {
    let c = colors();
    println!(
        "{}Image to WebP Converter (Rust){}\n\
         Usage: {program} <files...> [--max-size KB] [--jobs N]\n\n\
         Options:\n  \
         --max-size KB  Maximum output size per image in KiB (default: 300)\n  \
         --jobs N       Number of parallel workers (default: 4)\n  \
         --help         Show this help\n\n\
         Run without arguments to open the graphical interface.\n\n\
         Examples:\n  \
         {program} *.jpg *.heic\n  \
         {program} *.jpg *.png --max-size 300\n  \
         {program} *.heic --max-size 200 --jobs 6",
        c.bold, c.reset
    );
}

fn positive(value: Option<&String>) -> Option<u64> {
    value?.parse::<u64>().ok().filter(|&v| v > 0)
}

pub fn run(args: Vec<String>) -> ExitCode {
    let program = args.first().cloned().unwrap_or_else(|| "webp-forge".into());
    let c = colors();
    let mut cfg = Config::default();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut options = true;

    let mut it = args.iter().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--" if options => options = false,
            "--help" if options => {
                usage(&program);
                return ExitCode::SUCCESS;
            }
            "--max-size" if options => match positive(it.next()) {
                Some(v) if v <= u64::MAX / 1024 => cfg.max_kb = v,
                _ => {
                    eprintln!("{}[ERROR]{} --max-size requires a positive integer that is not too large.", c.red, c.reset);
                    return ExitCode::from(2);
                }
            },
            "--jobs" if options => match positive(it.next()) {
                Some(v) => cfg.jobs = v as usize,
                None => {
                    eprintln!("{}[ERROR]{} --jobs requires a positive integer.", c.red, c.reset);
                    return ExitCode::from(2);
                }
            },
            _ => files.push(PathBuf::from(arg)),
        }
    }

    if files.is_empty() {
        usage(&program);
        return ExitCode::from(2);
    }

    // Keep lines from different workers from interleaving.
    let console = Mutex::new(());
    let total = files.len();
    let done = std::sync::atomic::AtomicUsize::new(0);
    let emit = |event: Event| {
        let _guard = console.lock().unwrap_or_else(|e| e.into_inner());
        match event {
            Event::Log { level, text } => {
                let color = match level {
                    Level::Info | Level::Try => c.cyan,
                    Level::SizeOk | Level::Ok => c.green,
                    Level::SizeBig | Level::Warn => c.yellow,
                    Level::Error => c.red,
                };
                let line = format!("{color}[{}]{} {text}", level.tag(), c.reset);
                if level == Level::Error {
                    eprintln!("{line}");
                } else {
                    println!("{line}");
                }
            }
            Event::Done { .. } | Event::Failed { .. } => {
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                println!(
                    "{}[PROGRESS]{} {n}/{total} complete ({:.1}%)",
                    c.cyan,
                    c.reset,
                    100.0 * n as f64 / total as f64
                );
            }
            Event::Progress { .. } | Event::Finished { .. } => {}
        }
        let _ = std::io::stdout().flush();
    };

    let cancel = AtomicBool::new(false);
    let (ok, bad) = converter::run(&files, &cfg, &emit, &cancel);

    println!(
        "\n========================================\n\
         {}Conversion Summary{}\n\
         Total:     {total}\n\
         {}Converted: {ok}{}\n\
         {}Failed:    {bad}{}\n\
         Output:    {}/\n\
         ========================================",
        c.bold,
        c.reset,
        c.green,
        c.reset,
        c.red,
        c.reset,
        cfg.output_dir.display()
    );

    if bad == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
