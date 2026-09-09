use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};

use super::*;

#[test]
fn test_decode_image_rgba_returns_dimensions_and_rgba_bytes() {
    // Arrange
    let mut png_bytes = Vec::new();
    PngEncoder::new(&mut png_bytes)
        .write_image(
            &[255, 0, 0, 255, 0, 255, 0, 255],
            2,
            1,
            ExtendedColorType::Rgba8,
        )
        .expect("test PNG should encode");

    // Act
    let image_data = decode_image_rgba(&png_bytes, ImageFormat::Png).expect("PNG should decode");

    // Assert
    assert_eq!(image_data.width, 2);
    assert_eq!(image_data.height, 1);
    assert_eq!(image_data.rgba_bytes, vec![255, 0, 0, 255, 0, 255, 0, 255]);
}
