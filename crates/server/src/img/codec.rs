//! 像素数组与图片字节之间的唯一通道。
//! JPEG 用于 thumb/proxy（体积小、编码快），PNG 只保留上游给的就是 PNG 的场合（成图三件套、蒙版）。

use crate::error::{AppError, Result};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::metadata::Orientation;
use image::{DynamicImage, ExtendedColorType as ColorType, ImageDecoder, ImageEncoder, ImageReader};
use stitch_core::Rgba;
use std::io::Cursor;

/// 只接受已启用的普通图片格式；不设置文件体积、长边、像素或解码分配上限。
/// 图像任务仍由共同资源许可串行/限并发调度，实际容量取决于可用内存。
fn reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()
        .map_err(|e| AppError::detail("srv.image.badFormat", e))?;
    if !matches!(reader.format(), Some(image::ImageFormat::Jpeg | image::ImageFormat::Png | image::ImageFormat::WebP | image::ImageFormat::Bmp)) {
        return Err(AppError::bad("srv.image.unsupportedFormat"));
    }
    reader.no_limits();
    Ok(reader)
}

/// 只读真实尺寸与方向，不分配像素，导入时就建立可信的尺寸元数据。
pub(crate) fn dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    let mut decoder = reader(bytes)?.into_decoder().map_err(|e| AppError::detail("srv.image.decoderFail", e))?;
    let (w, h) = decoder.dimensions();
    let swap = matches!(decoder.orientation().unwrap_or(Orientation::NoTransforms), Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH);
    Ok(if swap { (h, w) } else { (w, h) })
}

/// 解码成 RGBA，并把 EXIF 方向烘焙进像素，保证缩略图与原图显示方向一致。
pub fn decode(bytes: &[u8]) -> Result<Rgba> {
    let mut decoder = reader(bytes)?.into_decoder().map_err(|e| AppError::detail("srv.image.decoderFail", e))?;
    // PNG/WEBP 的解码器没有 orientation()，默认就是 Unspecified；JPEG/TIFF 会真读
    let orient = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| AppError::detail("srv.image.decodeFail", e))?;
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

/// 把带 alpha 的画稿拍到纯色底上。
/// 画稿存的是"深色墨 + 透明底"的 RGBA PNG（无损、可继续改），而 `images/edits` 收的是不透明图，
/// 所以这一步只在提交时做，不动存着的那份。alpha=0 处 RGB 是 0（clearRect 的结果），
/// 按 alpha 混合后自然回到底色，不需要额外的墨色假设。
pub fn flatten(img: &Rgba, bg: [u8; 3]) -> Rgba {
    let mut out = img.clone();
    for p in out.px.chunks_exact_mut(4) {
        let a = p[3] as f32 / 255.0;
        let ia = 1.0 - a;
        for c in 0..3 {
            let v = p[c] as f32 * a + bg[c] as f32 * ia;
            p[c] = (v.round().clamp(0.0, 255.0)) as u8;
        }
        p[3] = 255;
    }
    out
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
    fn large_images_have_no_application_dimension_or_allocation_cap() {
        let img = Rgba::new(40_001, 1);
        let bytes = encode_png(&img);
        assert_eq!(dimensions(&bytes).unwrap(), (40_001, 1));
        assert_eq!(decode(&bytes).unwrap(), img);
        // 只读头部：超过旧像素门槛与解码器默认 512 MiB，不分配整幅像素。
        let mut bmp = vec![0u8; 54];
        bmp[..2].copy_from_slice(b"BM");
        bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&16_000u32.to_le_bytes());
        bmp[22..26].copy_from_slice(&10_000u32.to_le_bytes());
        bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
        bmp[28..30].copy_from_slice(&24u16.to_le_bytes());
        assert_eq!(dimensions(&bmp).unwrap(), (16_000, 10_000));
    }

    #[test]
    fn raw_tiff_and_non_image_content_are_rejected() {
        for bytes in [b"II\x2a\x00\x08\x00\x00\x00".as_slice(), b"not an image".as_slice()] {
            assert!(dimensions(bytes).is_err());
            assert!(decode(bytes).is_err());
        }
    }

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
    fn 画稿拍到白底_透明处回底色墨处不动() {
        let mut img = Rgba::new(2, 1);
        img.set(0, 0, [0, 0, 0, 0]);          // clearRect 之后的透明处 RGB 也是 0
        img.set(1, 0, [40, 40, 40, 255]);     // 实心的一笔
        let f = flatten(&img, [255, 255, 255]);
        assert_eq!(f.get(0, 0), [255, 255, 255, 255], "透明处该是纯白底");
        assert_eq!(f.get(1, 0), [40, 40, 40, 255], "墨色不该被底色冲淡");
        // 半覆盖的边按 alpha 混，不会留出透明
        img.set(0, 0, [20, 20, 20, 128]);
        let g = flatten(&img, [255, 255, 255]);
        assert!((g.get(0, 0)[0] as i32 - 138).abs() <= 1, "{:?}", g.get(0, 0));
        assert_eq!(g.get(0, 0)[3], 255);
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
