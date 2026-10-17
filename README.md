<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="logos/webp-forge-lockup-dark.png">
  <img alt="WebP Forge" src="logos/webp-forge-lockup-light.png" width="420">
</picture>

### Squeeze any image into a WebP that fits your size limit.

A native Rust port of the **Image to WebP Converter** — same engine as the original
C++ tool, wrapped in a fast, frameless desktop UI and a scriptable command line.

[![Rust](https://img.shields.io/badge/Rust-2024%20edition-000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/version-1.2.0-4DF0D4?style=flat-square)](#)
[![Platforms](https://img.shields.io/badge/platforms-Linux%20·%20macOS%20·%20Windows-4DF0D4?style=flat-square)](#)
[![Made by Araz Code](https://img.shields.io/badge/made%20by-Araz%20Code-4DF0D4?style=flat-square)](https://arazcode.com)

</div>

---

## ✨ What it does

Instead of guessing a single quality setting, WebP Forge **searches** for the best
result that still fits your limit. For every image it walks a ladder of maximum
widths and qualities, keeping the largest, sharpest output that stays under your
target size — automatically.

```
Original (4000×3000)  ──▶  width ladder × quality ladder  ──▶  output.webp  ≤ 300 KiB
                          3000px → 2600px → 2200px … 600px
                          Q90 → Q85 → Q80 → … Q40
```

- 🎯 **Guaranteed size target** — set a KiB budget and every result fits it (or fails loudly).
- 🖼️ **Reads PNG, JPEG, TIFF, WEBP and HEIC/HEIF** out of the box.
- ⚡ **Parallel batch conversion** with a configurable worker count.
- 🖥️ **Two front ends** — a polished desktop GUI, and a CLI that matches the original tool.
- 🧵 **Non-blocking UI** — conversion runs on background threads with live per-file progress.
- 🗂️ **Drag & drop folders** — drop a directory and its images are queued for you.

---

## 📸 The interface

WebP Forge opens as a frameless dark window with a configuration card, a drop zone
and active queue, and a collapsible terminal log.

| Area | What it does |
| --- | --- |
| **Configuration** | Output directory, max size per image (with quick 100/300/500 KB presets), parallel workers |
| **Drop Zone & Queue** | Drag & drop images or folders, browse files, live per-file status and progress bars |
| **Terminal Log** | Colour-coded, timestamped engine log you can collapse or clear |
| **Info tab** | Version and project credits |

> Run it with `cargo run --release` — no arguments opens the GUI.

---

## 🚀 Quick start

### 1. Install the external tools

WebP Forge drives well-known command-line tools rather than bundling codecs.
Install everything **once**:

| Tool | Needed for | Debian / Ubuntu | macOS (Homebrew) | Fedora (DNF) | Windows |
| --- | --- | --- | --- | --- | --- |
| `cwebp` | everything | `apt install webp` | `brew install webp` | `dnf install libwebp-tools` | `scoop install libwebp` |
| `identify` | reading image size | `apt install imagemagick` | `brew install imagemagick` | `dnf install ImageMagick` | `scoop install imagemagick` |
| `heif-convert` | HEIC / HEIF only | `apt install libheif-examples` | `brew install libheif` | `dnf install libheif-tools` | libheif tools |

> ImageMagick 7's `magick identify` is picked up automatically — no extra setup.

### 2. Build & run

```bash
cargo run --release                 # opens the GUI
```

That's it. On Linux you'll also need the usual GUI build dependencies (below).

---

## ⌨️ Command line

The CLI mirrors the original C++ tool exactly:

```bash
cargo run --release -- <files...> [--max-size KB] [--jobs N]
```

**Examples**

```bash
# Convert a handful of images with the defaults (300 KiB, 4 workers)
webp-forge *.jpg *.heic

# Tighter budget
webp-forge *.jpg *.png --max-size 300

# More parallelism for big batches
webp-forge *.heic --max-size 200 --jobs 6

# Use `--` to treat everything after it as file names
webp-forge -- --weird-name.jpg
```

**Options**

| Flag | Description | Default |
| --- | --- | --- |
| `--max-size KB` | Maximum output size per image, in KiB | `300` |
| `--jobs N` | Number of parallel workers | `4` |
| `--help` | Show usage and exit | — |
| `--` | Stop parsing options; the rest are file names | — |

Output is written to an `output/` directory next to where you run the command, and
the run ends with a colour-coded conversion summary.

> **Note:** CLI mode converts the exact files you list. To pull in every image from
> a folder, use the GUI's drag & drop (folders are expanded automatically).

---

## ⚙️ How the size search works

For each image the engine builds a **width ladder** — the original width, followed
by `3000, 2600, 2200, 2000, 1800, 1600, 1400, 1200, 1000, 800, 600`, plus a final
`400px` step for originals wider than 400px — and tries a **quality ladder** of
`90, 85, 80, 75, 70, 65, 60, 55, 50, 45, 40`.

It converts widest-and-best first and stops at the first result that fits the
budget, so you get the highest fidelity that still satisfies your limit. If even
the smallest, lowest-quality pass is too big, that image is reported as failed.

---

## 🔧 Building from source

**Requirements**

- Rust **2024 edition** (Rust 1.85+)
- The external tools listed above

**Linux GUI dependencies**

```bash
sudo apt install libxkbcommon-dev libwayland-dev \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
```

**Build**

```bash
cargo build --release      # binary at target/release/webp-forge
cargo test                 # run the test suite
```

The release profile is tuned for size and speed (`opt-level = 3`, thin LTO,
single codegen unit, symbols stripped).

---

## 🗂️ Project layout

```
webp-forge/
├── src/
│   ├── converter.rs   # conversion engine — no UI deps, emits Events
│   ├── cli.rs         # command-line front end
│   ├── gui.rs         # egui desktop UI (cards, drop zone, terminal log)
│   └── main.rs        # dispatches to GUI or CLI
├── logos/             # brand assets (icons, lockups, .ico)
├── Cargo.toml
└── README.md
```

The engine is intentionally UI-agnostic: it reports progress through a stream of
`Event`s (`Log`, `Progress`, `Done`, `Failed`, `Finished`), which the CLI renders
as coloured terminal output and the GUI renders as live queue rows.

---

## 🩺 Troubleshooting

| Symptom | Fix |
| --- | --- |
| `Missing command: cwebp` | Install the `webp` package (see the table above). |
| `Missing command: identify` | Install ImageMagick. |
| `heif-convert is missing` | Install the libheif tools to convert HEIC/HEIF. |
| Image keeps failing | Try a larger `--max-size`, or check the terminal log for the exact reason. |
| "Could not reach N KiB" | Even the smallest pass was too big — raise the limit. |
| Two inputs share a name | Rename one input; both would write the same `.webp`. |

---

## 👤 Credits

Designed and developed by **[Araz Code](https://arazcode.com)**.
Araz Code is a brand of **Novin Hushe Azerakhsh-e Araz LLC** ([nhazerakhsh.ir](https://nhazerakhsh.ir)).

📧 [info@arazcode.com](mailto:info@arazcode.com) · [info@nhazerakhsh.ir](mailto:info@nhazerakhsh.ir)
