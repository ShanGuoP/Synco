//! 像素数组与图片字节之间的唯一通道。
//! JPEG 用于 thumb/proxy（体积小、编码快），PNG 只保留上游给的就是 PNG 的场合（成图三件套、蒙版）。

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::metadata::Orientation;
use image::{DynamicImage, ExtendedColorType as ColorType, ImageDecoder, ImageEncoder, ImageReader};
use stitch_core::Rgba;
use std::io::Cursor;

/// 解码成 RGBA，并把 EXIF 方向烘焙进像素。
/// 浏览器渲染 `<img>` 时自己按 EXIF 转，我们的缩略图不转就会出现"原图正着看、缩略图横着躺"，
/// 所以这一步必须在服务端做，而不是指望前端。
pub fn decode(bytes: &[u8]) -> Result<Rgba, String> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("认不出图片格式：{e}"))?;
    let mut decoder = reader.into_decoder().map_err(|e| format!("建解码器失败：{e}"))?;
    // PNG/WEBP 的解码器没有 orientation()，默认就是 Unspecified；JPEG/TIFF 会真读
    let orient = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| format!("解码失败：{e}"))?;
    img.apply_orientation(orient);
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    Ok(Rgba::from_pixels(w, h, rgba.into_raw()))
}

pub fn encode_png(img: &Rgba) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    PngEncoder::new(&mut out)
        .write_image(&img.px, img.w as u32, img.h as u32, ColorType::Rgba8)
        .expect("PNG 编码不该失败");
    out.into_inner()
}

/// JPEG 没有 alpha 通道：这里丢掉 alpha 是有意的（thumb/proxy 只给人看）。
/// 蒙版绝对不能走这条——不透明=保留、透明=重绘全靠 alpha，那一路走 `encode_png`。
pub fn encode_jpeg(img: &Rgba, quality: u8) -> Vec<u8> {
    let rgb: Vec<u8> = img.px.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut out = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut out, quality.max(1))
        .write_image(&rgb, img.w as u32, img.h as u32, ColorType::Rgb8)
        .expect("JPEG 编码不该失败");
    out.into_inner()
}

/// 等比缩到长边不超过 edge（缩小时用面积核，不会出马赛克）
pub fn scale_to_long_edge(img: &Rgba, edge: usize) -> Rgba {
    let long = img.w.max(img.h);
    if long == 0 || long <= edge {
        return img.clone();
    }
    let k = edge as f64 / long as f64;
    let dw = ((img.w as f64) * k).round().max(1.0) as usize;
    let dh = ((img.h as f64) * k).round().max(1.0) as usize;
    stitch_core::resize_rgba(img, dw, dh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_jpeg_往回一致() {
        let mut img = Rgba::new(8, 6);
        for y in 0..6 {
            for x in 0..8 {
                img.set(x, y, [(x * 30) as u8, (y * 40) as u8, 128, 255]);
            }
        }
        let back = decode(&encode_png(&img)).unwrap();
        assert_eq!(back, img);

        let j = encode_jpeg(&img, 82);
        let back = decode(&j).unwrap();
        assert_eq!((back.w, back.h), (8, 6));
    }

    #[test]
    fn 长边缩放向上取整且不放大() {
        let img = Rgba::new(400, 200);
        let s = scale_to_long_edge(&img, 100);
        assert_eq!((s.w, s.h), (100, 50));
        assert_eq!(scale_to_long_edge(&img, 900).w, 400);
        // 0×0 的缓冲不该炸（导入失败的空图会走到这里）
        assert_eq!(scale_to_long_edge(&Rgba::new(0, 0), 100).w, 0);
    }
}
