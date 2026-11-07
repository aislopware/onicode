//! Turn preparation: attachments read from the disk, and the prompt they make. Tokens live in
//! `onicode-protocol`.
use std::io::Read;
use std::path::{Path, PathBuf};

pub use onicode_protocol::composer::*;

use crate::skills::DiscoveredSkill;

pub fn validate_attachments(paths: &[PathBuf]) -> Result<(), String> {
    if paths.len() > MAX_ATTACHMENTS {
        return Err(format!("Attach up to {MAX_ATTACHMENTS} files at a time."));
    }
    let mut total = 0;
    for path in paths {
        let meta = path
            .metadata()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if !meta.is_file() {
            return Err(format!("{} is not a file.", path.display()));
        }
        total += meta.len();
        if total > MAX_EMBED_BYTES {
            return Err("Attachments must total 20 MB or less.".into());
        }
    }
    Ok(())
}
pub fn image_data(path: &Path) -> Result<String, String> {
    use base64::Engine;
    let mut data = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_EMBED_BYTES + 1).read_to_end(&mut data))
        .map_err(|e| e.to_string())?;
    if data.len() as u64 > MAX_EMBED_BYTES {
        return Err("Image is larger than 20 MB.".into());
    }
    Ok(crate::b64::STANDARD.encode(data))
}
/// Decode a saved desktop image attachment off the UI thread.
pub fn decode_image_data(data: &str) -> Result<Vec<u8>, String> {
    if crate::remote::attachments::is_attachment_source(data) {
        return crate::remote::attachments::read_attachment_source(data);
    }
    use base64::Engine;
    let data = data
        .strip_prefix("data:")
        .and_then(|data| data.split_once(',').map(|(_, data)| data))
        .unwrap_or(data);
    if data.len() as u64 > MAX_EMBED_BYTES.div_ceil(3) * 4 {
        return Err("Image is larger than 20 MB.".into());
    }
    let bytes = crate::b64::STANDARD
        .decode(data)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_EMBED_BYTES {
        return Err("Image is larger than 20 MB.".into());
    }
    Ok(bytes)
}
/// Shrink an attachment until its shorter side just covers `side` pixels, so a
/// chip never holds a full screenshot in memory. BGRA, as GPU sprites expect.
pub fn thumbnail_bgra(bytes: &[u8], side: u32) -> Result<image::RgbaImage, String> {
    let image = image::load_from_memory(bytes).map_err(|error| error.to_string())?;
    let short = image.width().min(image.height()).max(1);
    Ok(scaled_bgra(image, f64::from(side) / f64::from(short)))
}
fn scaled_bgra(image: image::DynamicImage, scale: f64) -> image::RgbaImage {
    let (width, height) = (image.width(), image.height());
    let mut image = if scale < 1. {
        image.thumbnail_exact(
            ((f64::from(width) * scale).round() as u32).max(1),
            ((f64::from(height) * scale).round() as u32).max(1),
        )
    } else {
        image
    }
    .into_rgba8();
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    image
}
/// Resolve only exact, boundary-delimited known skill invocations. The original
/// draft remains the visible user message; these instructions travel on the wire.
pub fn prepare_prompt(text: &str, cwd: &str, attachments: &[PathBuf]) -> Result<String, String> {
    validate_attachments(attachments)?;
    let mut blocks = Vec::new();
    let mut context_bytes = 0;
    if text.split_whitespace().any(|word| word.starts_with('/')) {
        for skill in crate::skills::list_skills(cwd.to_owned(), None)? {
            if invoked(text, &skill) {
                let body = read_text(Path::new(&skill.path), 256 * 1024)?;
                context_bytes += body.len();
                if context_bytes > 512 * 1024 {
                    return Err(
                        "Selected skills and attachments exceed the 512 KB text context limit."
                            .into(),
                    );
                }
                blocks.push(format!("## /{}\nSkill location: {}\nFollow this skill's instructions; resolve its relative resources from its directory.\n\n{body}", skill.name, skill.path));
            }
        }
    }
    // Mentions use full project-relative paths, including quoted paths with spaces.
    let mentions = mentions(text);
    for relative in mentions {
        let path = Path::new(cwd).join(&relative);
        if let (Ok(root), Ok(path)) = (Path::new(cwd).canonicalize(), path.canonicalize()) {
            if path.starts_with(&root) && path.is_file() {
                blocks.push(format!("Referenced project file: {}", path.display()));
            }
        }
    }
    for path in attachments {
        let mut block = format!("Attached file: {}", path.display());
        if image_mime(path).is_none() {
            if let Ok(body) = read_text(path, 128 * 1024) {
                if !body.contains('\0') {
                    context_bytes += body.len();
                    if context_bytes > 512 * 1024 {
                        return Err(
                            "Selected skills and attachments exceed the 512 KB text context limit."
                                .into(),
                        );
                    }
                    block.push_str(&format!("\n<attached_file>\n{body}\n</attached_file>"));
                }
            }
        }
        blocks.push(block);
    }
    let text = if text.trim().is_empty() && !attachments.is_empty() {
        "The user attached these files. Use the conversation to determine what to do with them; ask if the intent is unclear."
    } else {
        text
    };
    if blocks.is_empty() {
        Ok(text.to_owned())
    } else {
        Ok(format!("{}\n\n---\n\n{text}", blocks.join("\n\n")))
    }
}
fn invoked(text: &str, skill: &DiscoveredSkill) -> bool {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('>'))
        .any(|line| {
            line.split_whitespace().any(|word| {
                word.strip_prefix('/').is_some_and(|name| {
                    name.trim_end_matches([',', '.', ';', '!', '?']) == skill.name
                })
            })
        })
}
fn read_text(path: &Path, max: usize) -> Result<String, String> {
    let mut data = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(max as u64 + 1).read_to_end(&mut data))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if data.len() > max {
        return Err(format!(
            "{} exceeds the text attachment limit ({max} bytes).",
            path.display()
        ));
    }
    String::from_utf8(data).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thumbnails_cover_the_chip_and_swap_to_bgra() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(2738, 1864, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let thumb = thumbnail_bgra(&png, 108).unwrap();
        assert_eq!(thumb.dimensions(), (159, 108));
        assert_eq!(thumb.get_pixel(0, 0).0, [30, 20, 10, 255]);
        let small = thumbnail_bgra(&png, 4000).unwrap();
        assert_eq!(small.dimensions(), (2738, 1864));
        assert!(thumbnail_bgra(b"not an image", 108).is_err());
    }
    #[test]
    fn skills_and_text_attachments_reach_the_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join(".agents/skills/native-test-skill")).unwrap();
        std::fs::write(
            root.join(".agents/skills/native-test-skill/SKILL.md"),
            "---\nname: native-test-skill\n---\nInspect before edits.",
        )
        .unwrap();
        let file = root.join("context.txt");
        std::fs::write(&file, "useful context").unwrap();
        let prompt =
            prepare_prompt("/native-test-skill do it", root.to_str().unwrap(), &[file]).unwrap();
        assert!(prompt.contains("Inspect before edits."));
        assert!(prompt.contains("useful context"));
        assert!(prompt.ends_with("/native-test-skill do it"));
    }
}
