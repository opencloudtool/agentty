//! Screenshot overlay rendering for visual failure reporting.
//!
//! Draws bounding boxes, labels, and color swatches onto copies of
//! screenshot PNGs so test failures show the located element directly
//! on the visual artifact.

use std::path::Path;

use image::{Rgba, RgbaImage};

use crate::calibration::{Calibration, PixelRect};
use crate::frame::CellColor;
use crate::region::Region;

/// An overlay renderer that draws annotations onto a screenshot image.
pub struct OverlayRenderer {
    /// The image being annotated.
    image: RgbaImage,
    /// The calibration mapping cells to pixels.
    calibration: Calibration,
}

impl OverlayRenderer {
    /// Create a new overlay renderer from a screenshot file and calibration.
    ///
    /// # Errors
    ///
    /// Returns an error if the image cannot be loaded.
    pub fn from_file(
        screenshot_path: &Path,
        calibration: Calibration,
    ) -> Result<Self, image::ImageError> {
        let image = image::open(screenshot_path)?.to_rgba8();

        Ok(Self { image, calibration })
    }

    /// Create a new overlay renderer from an existing image and calibration.
    pub fn from_image(image: RgbaImage, calibration: Calibration) -> Self {
        Self { image, calibration }
    }

    /// Draw a rectangular bounding box around a terminal region.
    pub fn draw_region_box(&mut self, region: &Region, color: CellColor, thickness: u32) {
        let pixel_rect = self.calibration.region_to_pixels(region);

        self.draw_rect_outline(&pixel_rect, color, thickness);
    }

    /// Draw a label at the top-left corner of a terminal region.
    ///
    /// Since we cannot render text without a font rasterizer, this draws
    /// a small colored indicator rectangle instead.
    pub fn draw_label_indicator(&mut self, region: &Region, color: CellColor) {
        let pixel_rect = self.calibration.region_to_pixels(region);
        let indicator = PixelRect {
            x_coordinate: pixel_rect.x_coordinate,
            y_coordinate: pixel_rect.y_coordinate.max(4.0) - 4.0,
            width: 20.0_f64.min(pixel_rect.width),
            height: 3.0,
        };

        self.fill_rect(&indicator, color);
    }

    /// Draw a color swatch (small filled square) at a position.
    pub fn draw_color_swatch(&mut self, pixel_x: u32, pixel_y: u32, color: CellColor) {
        let swatch = PixelRect {
            x_coordinate: f64::from(pixel_x),
            y_coordinate: f64::from(pixel_y),
            width: 12.0,
            height: 12.0,
        };

        self.fill_rect(&swatch, color);
    }

    /// Save the annotated image to a file.
    ///
    /// # Errors
    ///
    /// Returns an error if writing the image fails.
    pub fn save(&self, path: &Path) -> Result<(), image::ImageError> {
        self.image.save(path)?;

        Ok(())
    }

    /// Return the annotated image.
    pub fn into_image(self) -> RgbaImage {
        self.image
    }

    /// Draw a rectangle outline on the image.
    fn draw_rect_outline(&mut self, rect: &PixelRect, color: CellColor, thickness: u32) {
        let rgba = Rgba([color.red, color.green, color.blue, 255]);
        let (img_width, img_height) = self.image.dimensions();
        let (x_int, y_int, width_int, height_int) = rect.to_integer_bounds();

        // Top and bottom edges.
        for thickness_offset in 0..thickness {
            for x_pos in x_int..=(x_int + width_int).min(img_width - 1) {
                let top_y = (y_int + thickness_offset).min(img_height - 1);
                let bottom_y = (y_int + height_int)
                    .saturating_sub(thickness_offset)
                    .min(img_height - 1);
                self.image.put_pixel(x_pos.min(img_width - 1), top_y, rgba);
                self.image
                    .put_pixel(x_pos.min(img_width - 1), bottom_y, rgba);
            }
        }

        // Left and right edges.
        for thickness_offset in 0..thickness {
            for y_pos in y_int..=(y_int + height_int).min(img_height - 1) {
                let left_x = (x_int + thickness_offset).min(img_width - 1);
                let right_x = (x_int + width_int)
                    .saturating_sub(thickness_offset)
                    .min(img_width - 1);
                self.image
                    .put_pixel(left_x, y_pos.min(img_height - 1), rgba);
                self.image
                    .put_pixel(right_x, y_pos.min(img_height - 1), rgba);
            }
        }
    }

    /// Fill a rectangle with a solid color.
    fn fill_rect(&mut self, rect: &PixelRect, color: CellColor) {
        let rgba = Rgba([color.red, color.green, color.blue, 200]);
        let (img_width, img_height) = self.image.dimensions();
        let (x_int, y_int, width_int, height_int) = rect.to_integer_bounds();

        for y_pos in y_int..(y_int + height_int).min(img_height) {
            for x_pos in x_int..(x_int + width_int).min(img_width) {
                self.image.put_pixel(x_pos, y_pos, rgba);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_region_box_modifies_pixels() {
        // Arrange
        let image = RgbaImage::new(200, 100);
        let calibration = Calibration::new(0.0, 0.0, 10.0, 20.0);
        let mut renderer = OverlayRenderer::from_image(image, calibration);
        let region = Region::new(1, 1, 5, 2);

        // Act
        renderer.draw_region_box(&region, CellColor::new(255, 0, 0), 1);
        let result = renderer.into_image();

        // Assert — the pixel at the top-left of the box should be red.
        let pixel = result.get_pixel(10, 20);
        assert_eq!(pixel[0], 255); // Red channel.
        assert_eq!(pixel[1], 0);
        assert_eq!(pixel[2], 0);
    }

    #[test]
    fn fill_rect_fills_area() {
        // Arrange
        let image = RgbaImage::new(100, 100);
        let calibration = Calibration::new(0.0, 0.0, 10.0, 10.0);
        let mut renderer = OverlayRenderer::from_image(image, calibration);

        // Act
        renderer.draw_color_swatch(10, 10, CellColor::new(0, 255, 0));
        let result = renderer.into_image();

        // Assert
        let pixel = result.get_pixel(15, 15);
        assert_eq!(pixel[1], 255); // Green channel.
    }

    #[test]
    fn draw_label_indicator_above_region() {
        // Arrange
        let image = RgbaImage::new(200, 100);
        let calibration = Calibration::new(0.0, 0.0, 10.0, 20.0);
        let mut renderer = OverlayRenderer::from_image(image, calibration);
        let region = Region::new(2, 2, 5, 1);

        // Act
        renderer.draw_label_indicator(&region, CellColor::new(0, 0, 255));
        let result = renderer.into_image();

        // Assert — indicator should be above the region.
        let pixel = result.get_pixel(20, 37);
        assert_eq!(pixel[2], 255); // Blue channel.
    }
}
