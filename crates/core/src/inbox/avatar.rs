//! Small public provider avatars. No provider credentials leave their adapter.
use std::io::Cursor;
use std::time::Duration;

pub fn png(url: &str) -> Result<Vec<u8>, String> {
    let parsed = url::Url::parse(url).map_err(|_| "Invalid avatar URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Invalid avatar URL".into());
    }
    let response = ureq::get(url)
        .config()
        .timeout_global(Some(Duration::from_secs(8)))
        .build()
        .header("User-Agent", "Onicode")
        .call()
        .map_err(|_| "Avatar unavailable".to_string())?;
    let bytes = match response
        .into_body()
        .into_with_config()
        .limit(2 * 1024 * 1024)
        .read_to_vec()
    {
        Ok(bytes) => bytes,
        Err(ureq::Error::BodyExceedsLimit(_)) => return Err("Avatar is too large".into()),
        Err(_) => return Err("Avatar unavailable".into()),
    };
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "Invalid avatar".to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| "Invalid avatar".to_string())?
        .thumbnail(64, 64)
        .into_rgba8();
    crate::project_appearance::png_bytes(image)
}
