pub const TILE_SIZE: usize = 256;
#[derive(Clone, Copy)]
pub enum LayerKind {
    Boolean,
    Count,
    MinimumHeight,
}
pub fn reduce_2x2(values: [Option<u16>; 4], kind: LayerKind) -> Option<u16> {
    let i = values.into_iter().flatten();
    match kind {
        LayerKind::Boolean | LayerKind::Count => i.max(),
        LayerKind::MinimumHeight => i.min(),
    }
}
pub fn grayscale_png(values: &[u8]) -> Result<Vec<u8>, image::ImageError> {
    use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};
    let mut out = Vec::new();
    PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::NoFilter,
    )
    .write_image(
        values,
        TILE_SIZE as u32,
        TILE_SIZE as u32,
        ExtendedColorType::L8,
    )?;
    Ok(out)
}
pub fn etag(bytes: &[u8]) -> String {
    format!("\"{}\"", blake3::hash(bytes).to_hex())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn semantic_reducers() {
        let v = [Some(9), None, Some(3), Some(5)];
        assert_eq!(reduce_2x2(v, LayerKind::Count), Some(9));
        assert_eq!(reduce_2x2(v, LayerKind::MinimumHeight), Some(3));
    }
    #[test]
    fn png_signature() {
        let p = grayscale_png(&vec![0; TILE_SIZE * TILE_SIZE]).unwrap();
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
    }
}
