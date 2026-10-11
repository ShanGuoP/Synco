//! Bundled YuNet and canonical MediaPipe FaceMesh, CPU inference through tract.
use crate::{error::{AppError, Result}, img::codec, models::entity::Image, repo::adjust as radj, state::Ctx, util};
use photoedit_core::face;
use serde_json::{json, Value};
use stitch_core::Rgba;
use tract_onnx::prelude::*;
use std::sync::Arc;

const DET: usize = 640;
const MESH: usize = 192;
pub struct Engine { detector: Arc<TypedRunnableModel>, mesh: Arc<TypedRunnableModel> }
fn inference_error(e: impl std::fmt::Display) -> AppError { AppError::fail_detail("srv.face.inference", e) }
fn plan(bytes: &[u8], side: usize, outputs: Option<&[&str]>) -> Result<Arc<TypedRunnableModel>> {
    let mut model = tract_onnx::onnx().model_for_read(&mut std::io::Cursor::new(bytes)).map_err(inference_error)?;
    model.set_input_fact(0, f32::fact([1, 3, side, side]).into()).map_err(inference_error)?;
    if let Some(names) = outputs { model.select_outputs_by_name(names).map_err(inference_error)?; }
    model.into_optimized().and_then(|m| m.into_runnable()).map_err(inference_error)
}
impl Engine {
    pub fn new() -> Result<Self> {
        let names = ["cls_8", "cls_16", "cls_32", "obj_8", "obj_16", "obj_32", "bbox_8", "bbox_16", "bbox_32", "kps_8", "kps_16", "kps_32"];
        Ok(Self {
            detector: plan(include_bytes!("../../../../../assets/face/yunet.onnx"), DET, Some(&names))?,
            mesh: plan(include_bytes!("../../../../../assets/face/facemesh.onnx"), MESH, None)?,
        })
    }
    pub fn detect(&self, img: &Rgba) -> Result<Vec<face::FaceBox>> {
        let lb = face::letterbox(img.w, img.h, DET);
        let mut input = vec![0f32; 3 * DET * DET];
        for y in 0..DET { for x in 0..DET {
            let p = sample(img, (x as f32 - lb.1) / lb.0, (y as f32 - lb.2) / lb.0);
            for c in 0..3 { input[c * DET * DET + y * DET + x] = p[2 - c]; }
        }}
        let t = Tensor::from_shape(&[1, 3, DET, DET], &input).map_err(inference_error)?;
        let out = self.detector.run(tvec!(t.into())).map_err(inference_error)?;
        let arrays: Vec<Vec<f32>> = out.iter().map(|v| v.to_plain_array_view::<f32>().map(|s| s.iter().copied().collect()).map_err(inference_error)).collect::<Result<_>>()?;
        if arrays.len() != 12 { return Err(inference_error("YuNet requires twelve heads")); }
        let mut faces = Vec::new();
        for (i, stride) in [8, 16, 32].into_iter().enumerate() {
            faces.extend(face::decode_yunet(&arrays[i], &arrays[i+3], &arrays[i+6], &arrays[i+9], DET, stride, lb, 0.75).map_err(inference_error)?);
        }
        faces.retain(|f| f.x < img.w as f32 && f.y < img.h as f32 && f.x + f.w > 0.0 && f.y + f.h > 0.0);
        let mut faces = face::nms(&faces, 0.3);
        faces.sort_by(|a,b| b.area().total_cmp(&a.area()));
        faces.truncate(16);
        Ok(faces)
    }
    pub fn landmarks(&self, img: &Rgba, f: &face::FaceBox) -> Result<Vec<[f32; 2]>> {
        let mut eyes = [f.points[1], f.points[2]];
        eyes.sort_by(|a,b| a[0].total_cmp(&b[0]));
        let angle = (eyes[1][1] - eyes[0][1]).atan2(eyes[1][0] - eyes[0][0]);
        let (sn, cs) = angle.sin_cos();
        let side = f.w.max(f.h) * 1.5;
        let center = f.center();
        let remap = |x: f32, y: f32| {
            let (dx, dy) = ((x / MESH as f32 - 0.5) * side, (y / MESH as f32 - 0.5) * side);
            [center[0] + cs * dx - sn * dy, center[1] + sn * dx + cs * dy]
        };
        let mut input = vec![0f32; 3 * MESH * MESH];
        for y in 0..MESH { for x in 0..MESH {
            let p = remap(x as f32, y as f32);
            let rgb = sample(img, p[0], p[1]);
            for c in 0..3 { input[c * MESH * MESH + y * MESH + x] = rgb[c] / 255.0; }
        }}
        let t = Tensor::from_shape(&[1, 3, MESH, MESH], &input).map_err(inference_error)?;
        let out = self.mesh.run(tvec!(t.into())).map_err(inference_error)?;
        if out.len() != 2 { return Err(inference_error("FaceMesh requires landmarks and presence")); }
        let points = out[0].to_plain_array_view::<f32>().map_err(inference_error)?.iter().copied().collect::<Vec<_>>();
        let score = out[1].to_plain_array_view::<f32>().map_err(inference_error)?.iter().copied().collect::<Vec<_>>();
        if points.len() != 468 * 3 || score.len() != 1 { return Err(inference_error("Unexpected FaceMesh output shape")); }
        // sigmoid(logit) >= 0.5 iff logit >= 0.
        if !score[0].is_finite() || score[0] < 0.0 { return Ok(Vec::new()); }
        let points: Vec<_> = points.chunks_exact(3).map(|p| {
            let q = remap(p[0], p[1]); [q[0] / img.w as f32, q[1] / img.h as f32]
        }).collect();
        if !points.iter().flatten().all(|x| x.is_finite()) { return Err(inference_error("Non-finite landmarks")); }
        Ok(points)
    }
}
// Bilinear RGB sampling with zero padding, matching the model's ROI contract.
fn sample(img: &Rgba, x: f32, y: f32) -> [f32; 3] {
    let (ix, iy) = (x.floor() as isize, y.floor() as isize);
    let (dx, dy) = (x - x.floor(), y - y.floor());
    let mut out = [0.0; 3];
    for (xx, yy, weight) in [(ix,iy,(1.0-dx)*(1.0-dy)),(ix+1,iy,dx*(1.0-dy)),(ix,iy+1,(1.0-dx)*dy),(ix+1,iy+1,dx*dy)] {
        if xx >= 0 && yy >= 0 && xx < img.w as isize && yy < img.h as isize {
            for c in 0..3 { out[c] += img.px[(yy as usize * img.w + xx as usize) * 4 + c] as f32 * weight; }
        }
    }
    out
}
/// Explicit refresh; plans load once, boxes and meshes commit together.
pub fn analyze(ctx: &Ctx, img: &Image) -> Result<Value> {
    let mut engine = ctx.face_engine.lock().unwrap_or_else(|e| e.into_inner());
    if engine.is_none() { *engine = Some(Engine::new()?); }
    let engine = engine.as_ref().unwrap();
    let rel = img.proxy_path.as_deref().filter(|r| util::data_file(&ctx.data,r).is_some_and(|p| p.is_file())).unwrap_or(&img.orig_path);
    let path = util::data_file(&ctx.data, rel).ok_or_else(|| AppError::bad("srv.image.origOutside"))?;
    let base = codec::decode(&std::fs::read(path).map_err(inference_error)?)?;
    let base = codec::scale_to_long_edge(&base, 1600);
    let found = engine.detect(&base)?;
    let mut boxes = Vec::new(); let mut meshes = Vec::new();
    for f in &found {
        let points: Vec<_> = f.points.iter().map(|p| [p[0] / base.w as f32, p[1] / base.h as f32]).collect();
        boxes.push(json!({"box": [f.x/base.w as f32,f.y/base.h as f32,f.w/base.w as f32,f.h/base.h as f32], "points5": points, "score": f.score}));
        meshes.push(json!({"points": engine.landmarks(&base, f)?}));
    }
    if meshes.is_empty() { meshes.push(json!({"points": []})); }
    radj::put_analysis(ctx, img.id, &boxes, &meshes)?;
    super::cached_analysis(ctx, img)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_models_load_and_reject_blank_image() {
        let engine = Engine::new().unwrap();
        assert!(engine.detect(&Rgba::new(320,240)).unwrap().is_empty());
    }
    /// Fetch a consented/local test portrait into the given path, then run:
    /// SYNCO_FACE_TEST_IMAGE=<path> cargo test -p synco-server real_portrait -- --ignored
    #[test]
    #[ignore = "requires SYNCO_FACE_TEST_IMAGE portrait fixture"]
    fn real_portrait_detects_dense_mesh_and_shapes() {
        let path = std::env::var("SYNCO_FACE_TEST_IMAGE").expect("set portrait path");
        let img = codec::decode(&std::fs::read(path).unwrap()).unwrap();
        let engine = Engine::new().unwrap();
        let faces = engine.detect(&img).unwrap();
        assert!(!faces.is_empty());
        let points = engine.landmarks(&img,&faces[0]).unwrap();
        assert_eq!(points.len(),468);
        let shape = face::shape_from_mesh(&points).unwrap();
        assert!(shape.iris_l[0] < shape.iris_r[0]);
        assert!(shape.chin[1] > shape.iris_l[1]);
        let mask = face::mesh_beauty_region(img.w,img.h,&[points]);
        assert_eq!(mask.max(),255);
        assert_eq!(mask.get(0,0),0);
        let mut auto = photoedit_core::ops::Auto::default();
        auto.face_slim.0=20; auto.eye_big.0=12; auto.nose_slim.0=10; auto.chin.0=8;
        let out = photoedit_core::warp::mls(&img,&auto,&shape);
        assert_ne!(out,img);
        assert_eq!(out.get(0,0),img.get(0,0));
        // Two portraits must remain separate detections with separate meshes.
        let mut pair=Rgba::new(img.w*2,img.h);
        for y in 0..img.h {
            let row=&img.px[y*img.w*4..(y+1)*img.w*4];
            let dst=&mut pair.px[y*img.w*8..(y+1)*img.w*8];
            dst[..img.w*4].copy_from_slice(row); dst[img.w*4..].copy_from_slice(row);
        }
        let two=engine.detect(&pair).unwrap();
        assert_eq!(two.len(),2);
        for f in &two { assert_eq!(engine.landmarks(&pair,f).unwrap().len(),468); }
    }
}
