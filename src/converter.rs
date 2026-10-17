//! Core conversion engine — a Rust port of the original C++ batch converter.
//!
//! It shells out (never through a shell) to `cwebp`, ImageMagick's `identify`
//! and, for HEIC/HEIF input, `heif-convert`. For every image it walks a ladder
//! of maximum widths and JPEG-style qualities until the WebP output fits the
//! requested size limit.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub const QUALITIES: [u8; 11] = [90, 85, 80, 75, 70, 65, 60, 55, 50, 45, 40];
pub const WIDTH_STEPS: [u64; 11] = [3000, 2600, 2200, 2000, 1800, 1600, 1400, 1200, 1000, 800, 600];

pub const SUPPORTED_EXTENSIONS: [&str; 8] = ["png", "jpg", "jpeg", "tif", "tiff", "webp", "heic", "heif"];

#[derive(Clone, Debug)]
pub struct Config {
    pub max_kb: u64,
    pub jobs: usize,
    pub output_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self { max_kb: 300, jobs: 4, output_dir: PathBuf::from("output") }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Try,
    SizeOk,
    SizeBig,
    Ok,
    Warn,
    Error,
}

impl Level {
    pub fn tag(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Try => "*",
            Level::SizeOk | Level::SizeBig => "SIZE",
            Level::Ok => "OK",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// Everything the engine reports back to the CLI / GUI. `index` is 0-based and
/// refers to the position in the list of files passed to [`run`].
#[derive(Clone, Debug)]
pub enum Event {
    Log { level: Level, text: String },
    Progress { index: usize, width: u64, quality: u8, fraction: f32 },
    #[allow(dead_code)]
    Done { index: usize, size: u64, quality: u8, width: u64, output: PathBuf },
    Failed { index: usize, reason: String },
    Finished { converted: usize, failed: usize },
}

pub type Emit<'a> = &'a (dyn Fn(Event) + Sync);

pub fn kib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1024.0)
}

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| SUPPORTED_EXTENSIONS.contains(&e.as_str()))
}

/// Extension patterns for a file-chooser filter. Both spellings are listed
/// because choosers on some platforms match the `*.ext` globs case-sensitively
/// (Linux/GTK), which would otherwise hide `PHOTO.JPG` or `IMG.HEIC`.
pub fn filter_extensions() -> Vec<String> {
    SUPPORTED_EXTENSIONS
        .iter()
        .flat_map(|ext| [(*ext).to_string(), ext.to_uppercase()])
        .collect()
}

// ---------------------------------------------------------------------------
// External tools
// ---------------------------------------------------------------------------

pub struct Tools {
    cwebp: PathBuf,
    identify: PathBuf,
    identify_prefix: Vec<&'static str>,
    heif_convert: Option<PathBuf>,
}

impl Tools {
    pub fn locate() -> Result<Self, String> {
        let cwebp = which::which("cwebp").map_err(|_| "Missing command: cwebp".to_string())?;
        let (identify, identify_prefix) = if let Ok(p) = which::which("identify") {
            (p, vec![])
        } else if let Ok(p) = which::which("magick") {
            (p, vec!["identify"])
        } else {
            return Err("Missing command: identify (usually provided by ImageMagick)".into());
        };
        Ok(Self { cwebp, identify, identify_prefix, heif_convert: which::which("heif-convert").ok() })
    }
}

fn command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// The permissions a normally created file would get (0666 & !umask).
#[cfg(unix)]
fn default_file_mode() -> u32 {
    // SAFETY: umask is process-global; we read it once, before any worker starts.
    unsafe {
        let current = libc::umask(0);
        libc::umask(current);
        0o666 & !(current as u32)
    }
}

#[cfg(not(unix))]
fn default_file_mode() -> u32 {
    0o644
}

// ---------------------------------------------------------------------------
// Per-image work
// ---------------------------------------------------------------------------

struct Ctx<'a> {
    cfg: &'a Config,
    tools: &'a Tools,
    mode: u32,
    total: usize,
    emit: Emit<'a>,
    cancel: &'a AtomicBool,
}

struct Success {
    size: u64,
    quality: u8,
    width: u64,
    output: PathBuf,
}

fn read_dimensions(tools: &Tools, input: &Path) -> Result<(u64, u64), String> {
    let mut arg = input.as_os_str().to_owned();
    arg.push("[0]");
    let out = command(&tools.identify)
        .args(&tools.identify_prefix)
        .args(["-ping", "-format", "%w %h"])
        .arg(arg)
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("Could not run identify: {e}"))?;
    if !out.status.success() {
        return Err("Could not detect image dimensions.".into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.split_whitespace().map(|s| s.parse::<u64>());
    match (parts.next(), parts.next()) {
        (Some(Ok(w)), Some(Ok(h))) if w > 0 && h > 0 => Ok((w, h)),
        _ => Err("Invalid image dimensions returned by identify.".into()),
    }
}

fn find_converted_png(dir: &Path) -> Option<PathBuf> {
    let exact = dir.join("converted.png");
    if exact.is_file() {
        return Some(exact);
    }
    // Multi-image HEIC files are written as converted-1.png, converted-2.png, ...
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
        .collect();
    found.sort();
    found.into_iter().next()
}

fn process_image(ctx: &Ctx, index: usize, original: &Path) -> Result<Success, String> {
    let prefix = format!("[{}/{}] ", index + 1, ctx.total);
    let say = |level: Level, msg: String| {
        (ctx.emit)(Event::Log { level, text: format!("{prefix}{msg}") });
    };

    say(Level::Info, format!("Processing: {}", original.display()));

    let is_file = fs::metadata(original).map(|m| m.is_file()).unwrap_or(false);
    if !is_file || fs::File::open(original).is_err() {
        return Err(format!(
            "File does not exist, is not a regular file, or is not readable: {}",
            original.display()
        ));
    }

    let ext = original
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let stem = original
        .file_stem()
        .ok_or("Input has no file name.")?
        .to_string_lossy()
        .into_owned();
    let output = ctx.cfg.output_dir.join(format!("{stem}.webp"));

    // Absolute paths keep names starting with '-' from being read as options.
    let abs_original = std::path::absolute(original).unwrap_or_else(|_| original.to_path_buf());
    let mut input = abs_original.clone();
    let mut _heic_dir = None; // keeps the temporary PNG alive until we are done

    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "tif" | "tiff" | "webp" => {
            say(Level::Info, format!("Detected format: {ext}"));
        }
        "heic" | "heif" => {
            let tool = ctx
                .tools
                .heif_convert
                .as_ref()
                .ok_or("heif-convert is missing. Install the libheif tools.")?;
            let dir = tempfile::Builder::new()
                .prefix(".image-to-webp-")
                .tempdir()
                .map_err(|e| format!("Could not create temporary directory: {e}"))?;
            say(Level::Info, "Converting HEIC/HEIF to temporary PNG...".into());
            let status = command(tool)
                .arg(&abs_original)
                .arg(dir.path().join("converted.png"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| format!("HEIC conversion failed: {e}"))?;
            if !status.success() {
                return Err("HEIC conversion failed.".into());
            }
            input = find_converted_png(dir.path()).ok_or("HEIC conversion produced no PNG output.")?;
            _heic_dir = Some(dir);
        }
        "" => return Err("Unsupported format: (no extension)".into()),
        other => return Err(format!("Unsupported format: {other}")),
    }

    let (original_width, original_height) = read_dimensions(ctx.tools, &input)?;
    say(Level::Info, format!("Original dimensions: {original_width}x{original_height}"));

    let mut widths = vec![original_width];
    widths.extend(WIDTH_STEPS.iter().copied().filter(|&w| w < original_width));
    if original_width > 400 {
        widths.push(400);
    }

    let temp = tempfile::Builder::new()
        .prefix(".image-to-webp-")
        .suffix(".webp")
        .tempfile_in(&ctx.cfg.output_dir)
        .map_err(|e| format!("Could not create temporary file: {e}"))?;

    let max_bytes = ctx.cfg.max_kb * 1024;
    let total_attempts = (widths.len() * QUALITIES.len()) as f32;
    let mut attempts = 0usize;
    let mut chosen: Option<(u8, u64, u64)> = None; // (quality, width, size)

    'search: for &width in &widths {
        say(Level::Info, format!("Trying maximum width: {width}px"));
        for &quality in &QUALITIES {
            if ctx.cancel.load(Ordering::Relaxed) {
                return Err("Cancelled.".into());
            }
            say(Level::Try, format!("Trying width={width}px, quality={quality}"));
            (ctx.emit)(Event::Progress {
                index,
                width,
                quality,
                fraction: (attempts as f32 / total_attempts).min(0.95),
            });
            attempts += 1;

            let mut cmd = command(&ctx.tools.cwebp);
            cmd.args(["-quiet", "-q", &quality.to_string()]);
            if width != original_width {
                cmd.args(["-resize", &width.to_string(), "0"]);
            }
            cmd.arg(&input).arg("-o").arg(temp.path());
            let status = cmd
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| format!("Could not start cwebp: {e}"))?;
            if !status.success() {
                return Err(if width == original_width {
                    "WebP conversion failed.".into()
                } else {
                    "Resize/conversion failed.".into()
                });
            }

            let size = fs::metadata(temp.path())
                .map_err(|e| format!("Could not read temporary output size: {e}"))?
                .len();
            let detail = format!("width={width}px quality={quality} => {} KiB", kib(size));
            if size <= max_bytes {
                say(Level::SizeOk, format!("{detail} (within limit)"));
                chosen = Some((quality, width, size));
                break 'search;
            }
            say(Level::SizeBig, format!("{detail} (too large)"));
        }
    }

    let Some((quality, width, size)) = chosen else {
        say(Level::Warn, format!("Could not reach {} KiB for {}", ctx.cfg.max_kb, original.display()));
        return Err(format!("Could not reach {} KiB", ctx.cfg.max_kb));
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(ctx.mode))
            .map_err(|e| format!("Could not set output permissions: {e}"))?;
    }
    #[cfg(not(unix))]
    let _ = ctx.mode;

    temp.persist(&output)
        .map_err(|e| format!("Could not save {}: {}", output.display(), e.error))?;

    say(
        Level::Ok,
        format!(
            "Saved: {} | Size: {} KiB | Quality: {quality} | Max width: {width}px",
            output.display(),
            kib(size)
        ),
    );
    Ok(Success { size, quality, width, output })
}

// ---------------------------------------------------------------------------
// Batch driver
// ---------------------------------------------------------------------------

/// Converts all `files`, reporting through `emit`. Returns `(converted, failed)`.
pub fn run(files: &[PathBuf], cfg: &Config, emit: Emit, cancel: &AtomicBool) -> (usize, usize) {
    let total = files.len();
    let fail_all = |message: String| {
        emit(Event::Log { level: Level::Error, text: message.clone() });
        for index in 0..total {
            emit(Event::Failed { index, reason: message.clone() });
        }
        emit(Event::Finished { converted: 0, failed: total });
        (0, total)
    };

    let tools = match Tools::locate() {
        Ok(t) => t,
        Err(message) => return fail_all(message),
    };

    if let Err(e) = fs::create_dir_all(&cfg.output_dir) {
        return fail_all(format!("Could not create output directory '{}': {e}", cfg.output_dir.display()));
    }

    // Avoid concurrent overwrites when different folders contain the same basename.
    let mut names: Vec<PathBuf> = files
        .iter()
        .filter_map(|f| f.file_stem().map(|s| cfg.output_dir.join(format!("{}.webp", s.to_string_lossy()))))
        .collect();
    names.sort();
    if let Some(pair) = names.windows(2).find(|w| w[0] == w[1]) {
        return fail_all(format!(
            "Two or more inputs would write to the same output file: {}. Rename one input or process them separately.",
            pair[0].display()
        ));
    }

    let workers = cfg.jobs.max(1).min(total.max(1));
    emit(Event::Log {
        level: Level::Info,
        text: format!(
            "Files: {total} | Output: {}/ | Size limit: {} KiB | Workers: {workers}",
            cfg.output_dir.display(),
            cfg.max_kb
        ),
    });

    let ctx = Ctx { cfg, tools: &tools, mode: default_file_mode(), total, emit, cancel };
    let next = AtomicUsize::new(0);
    let converted = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= total {
                    break;
                }
                if cancel.load(Ordering::Relaxed) {
                    failed.fetch_add(1, Ordering::Relaxed);
                    emit(Event::Failed { index, reason: "Cancelled.".into() });
                    continue;
                }
                // A panic in one image must not take the whole batch down.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process_image(&ctx, index, &files[index])
                }))
                .unwrap_or_else(|_| Err("Unexpected internal error.".into()));

                match result {
                    Ok(done) => {
                        converted.fetch_add(1, Ordering::Relaxed);
                        emit(Event::Done {
                            index,
                            size: done.size,
                            quality: done.quality,
                            width: done.width,
                            output: done.output,
                        });
                    }
                    Err(reason) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                        emit(Event::Log {
                            level: Level::Error,
                            text: format!("[{}/{total}] {reason}", index + 1),
                        });
                        emit(Event::Failed { index, reason });
                    }
                }
            });
        }
    });

    let (ok, bad) = (converted.load(Ordering::Relaxed), failed.load(Ordering::Relaxed));
    emit(Event::Finished { converted: ok, failed: bad });
    (ok, bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_extensions_ignore_case() {
        for name in [
            "a.JPG", "a.Jpg", "a.jpeg", "a.JPEG", "a.Png", "a.HEIC", "a.Heif", "a.WEBP", "a.TIF",
            "a.TIFF", "a.jpg", "a.heic",
        ] {
            assert!(is_supported(Path::new(name)), "{name} should be supported");
        }
        for name in ["a.gif", "a.bmp", "a.avif", "a.txt", "a", "noext"] {
            assert!(!is_supported(Path::new(name)), "{name} should not be supported");
        }
    }

    #[test]
    fn filter_extensions_cover_both_cases() {
        let patterns = filter_extensions();
        for ext in SUPPORTED_EXTENSIONS {
            assert!(patterns.iter().any(|p| p == ext), "missing {ext}");
            assert!(patterns.iter().any(|p| p == &ext.to_uppercase()), "missing {ext} upper");
        }
        assert_eq!(patterns.len(), SUPPORTED_EXTENSIONS.len() * 2);
    }
}
