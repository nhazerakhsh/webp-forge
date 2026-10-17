//! Core conversion engine — fully Cargo-native.
//!
//! Decodes PNG, JPEG, TIFF and WEBP with the `image` crate, HEIC/HEIF with the
//! pure-Rust `heic` crate (optional `heic` feature), resizes with `imageops`,
//! and encodes lossy WebP straight to `libwebp-sys` (built from source by Cargo,
//! so no separate `cwebp` installation is required). It keeps the same ladder
//! of maximum widths and JPEG-style qualities until the WebP output fits the
//! requested size limit.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use image::imageops::{self, FilterType};
use image::RgbaImage;
use libwebp_sys::*;

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
// Per-image work
// ---------------------------------------------------------------------------

struct Ctx<'a> {
    cfg: &'a Config,
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

/// Decode the input file into an RGBA8 image.
fn decode_image(bytes: &[u8], ext: &str) -> Result<RgbaImage, String> {
    match ext {
        "heic" | "heif" => {
            #[cfg(feature = "heic")]
            {
                let out = heic::DecoderConfig::new()
                    .decode(bytes, heic::PixelLayout::Rgba8)
                    .map_err(|e| format!("HEIC/HEIF decode failed: {e}"))?;
                RgbaImage::from_raw(out.width, out.height, out.data)
                    .ok_or_else(|| "HEIC/HEIF produced invalid image dimensions.".to_string())
            }
            #[cfg(not(feature = "heic"))]
            {
                let _ = bytes;
                Err("HEIC/HEIF support is not enabled. Rebuild with `--features heic`.".to_string())
            }
        }
        "png" | "jpg" | "jpeg" | "tif" | "tiff" | "webp" => {
            let img = image::load_from_memory(bytes).map_err(|e| format!("Could not decode image: {e}"))?;
            Ok(img.to_rgba8())
        }
        "" => Err("Unsupported format: (no extension)".to_string()),
        other => Err(format!("Unsupported format: {other}")),
    }
}

/// Encode an RGBA8 buffer as lossy WebP using libwebp-sys directly.
///
/// `libwebp-sys` builds the libwebp C codec from source via Cargo, so no
/// separate `cwebp` installation is needed — only a C compiler.
fn encode_lossy_webp(rgba: &[u8], width: u32, height: u32, quality: f32) -> Result<Vec<u8>, String> {
    unsafe {
        let mut out_ptr: *mut u8 = std::ptr::null_mut();
        let size = WebPEncodeRGBA(
            rgba.as_ptr(),
            width as i32,
            height as i32,
            width as i32 * 4,
            quality,
            &mut out_ptr,
        );
        if out_ptr.is_null() {
            return Err(format!("WebP encode returned no output (size={size})"));
        }
        let bytes = std::slice::from_raw_parts(out_ptr, size).to_vec();
        WebPFree(out_ptr as *mut std::ffi::c_void);
        Ok(bytes)
    }
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
    let bytes = fs::read(&abs_original)
        .map_err(|e| format!("Could not read input file: {e}"))?;

    let orig_img = decode_image(&bytes, &ext)?;
    let (original_width, original_height) = orig_img.dimensions();
    say(Level::Info, format!("Detected format: {ext}"));
    say(Level::Info, format!("Original dimensions: {original_width}x{original_height}"));

    let mut widths: Vec<u64> = vec![original_width as u64];
    widths.extend(WIDTH_STEPS.iter().copied().filter(|&w| w < original_width as u64));
    if original_width > 400 {
        widths.push(400);
    }

    let max_bytes = ctx.cfg.max_kb * 1024;
    let total_attempts = (widths.len() * QUALITIES.len()) as f32;
    let mut attempts = 0usize;
    let mut chosen: Option<(u8, u64, u64, Vec<u8>)> = None; // (quality, width, size, bytes)

    'search: for &width in &widths {
        let new_height = if width == original_width as u64 {
            original_height
        } else {
            ((original_height as f64) * (width as f64) / (original_width as f64))
                .round()
                .max(1.0) as u32
        };
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

            let encoded = if width == original_width as u64 {
                encode_lossy_webp(orig_img.as_raw(), original_width, original_height, quality as f32)
                    .map_err(|e| format!("WebP conversion failed: {e}"))?
            } else {
                let resized = imageops::resize(&orig_img, width as u32, new_height, FilterType::Lanczos3);
                encode_lossy_webp(resized.as_raw(), resized.width(), resized.height(), quality as f32)
                    .map_err(|e| format!("Resize/conversion failed: {e}"))?
            };

            let size = encoded.len() as u64;
            let detail = format!("width={width}px quality={quality} => {} KiB", kib(size));
            if size <= max_bytes {
                say(Level::SizeOk, format!("{detail} (within limit)"));
                chosen = Some((quality, width, size, encoded));
                break 'search;
            }
            say(Level::SizeBig, format!("{detail} (too large)"));
        }
    }

    let Some((quality, width, size, encoded)) = chosen else {
        say(Level::Warn, format!("Could not reach {} KiB for {}", ctx.cfg.max_kb, original.display()));
        return Err(format!("Could not reach {} KiB", ctx.cfg.max_kb));
    };

    fs::write(&output, encoded)
        .map_err(|e| format!("Could not save {}: {}", output.display(), e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&output, fs::Permissions::from_mode(ctx.mode))
            .map_err(|e| format!("Could not set output permissions: {e}"))?;
    }
    #[cfg(not(unix))]
    let _ = ctx.mode;

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

    let ctx = Ctx { cfg, mode: default_file_mode(), total, emit, cancel };
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
