/// Print the Cosmic Capture Kit metadata embedded in a capture file (`--inspect`).
/// Reads a PNG `Comment` text chunk for screenshots, or the mp4/mkv `comment` tag
/// (via `ffprobe`) for recordings.
pub fn inspect(path: &std::path::Path) {
    let is_png = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png"));
    let meta = if is_png {
        crate::media::png::read_png_metadata(path)
    } else {
        ffprobe_comment(path)
    };
    match meta {
        Some(m) if m.starts_with("Cosmic Capture Kit") => {
            // The string is " | "-joined `key=value` pairs; one per line reads nicely.
            for (i, part) in m.split(" | ").enumerate() {
                if i == 0 {
                    println!("{part}");
                } else {
                    println!("  {part}");
                }
            }
        }
        Some(m) if !m.trim().is_empty() => println!("{m}"),
        _ => eprintln!(
            "No Cosmic Capture Kit metadata found in {}",
            path.display()
        ),
    }
}

/// The mp4/mkv `comment` tag via ffprobe (empty/`None` if absent or ffprobe missing).
fn ffprobe_comment(path: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new(crate::util::ffprobe_path())
        .args([
            "-v", "error",
            "-show_entries", "format_tags=comment",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}
