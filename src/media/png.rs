//! PNG read/write with text-chunk metadata.
//!
//! Screenshots embed their capture provenance as a `Comment` text chunk (read
//! back by `--inspect`). We go through the `png` crate directly because `image`
//! doesn't expose text chunks.

use image::RgbaImage;

/// Save an RGBA image as PNG, embedding `metadata` as a `Comment` text chunk (read
/// back by `--inspect`). We encode with the `png` crate directly because `image`
/// doesn't expose text chunks. Empty `metadata` writes no chunk.
pub fn save_png(img: &RgbaImage, path: &std::path::Path, metadata: &str) -> bool {
    let Ok(file) = std::fs::File::create(path) else {
        return false;
    };
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), img.width(), img.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    if !metadata.is_empty() {
        let _ = encoder.add_text_chunk("Comment".to_string(), metadata.to_string());
    }
    match encoder.write_header() {
        Ok(mut writer) => writer.write_image_data(img.as_raw()).is_ok(),
        Err(_) => false,
    }
}

/// Read the `Comment` text chunk embedded by [`save_png`] (used by `--inspect`).
pub fn read_png_metadata(path: &std::path::Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let reader = png::Decoder::new(std::io::BufReader::new(file)).read_info().ok()?;
    let info = reader.info();
    info.uncompressed_latin1_text
        .iter()
        .find(|t| t.keyword == "Comment")
        .map(|t| t.text.clone())
        .or_else(|| info.utf8_text.iter().find(|t| t.keyword == "Comment").and_then(|t| t.get_text().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screenshot's metadata survives a save → read round trip via the PNG
    /// `Comment` text chunk (what `--inspect` reads back).
    #[test]
    fn png_metadata_roundtrip() {
        let img = RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]));
        let path = std::env::temp_dir().join("cck-meta-roundtrip.png");
        let meta = "Cosmic Capture Kit | type=photo | source=cosmic | mode=region | cursor=off";
        assert!(save_png(&img, &path, meta));
        assert_eq!(read_png_metadata(&path).as_deref(), Some(meta));
        let _ = std::fs::remove_file(&path);
    }
}
