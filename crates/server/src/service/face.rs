//! Cached face analysis, in normalized source-image coordinates.
mod inference;
pub use inference::{analyze, Engine};
use crate::{error::Result, models::entity::Image, repo::adjust as radj, state::Ctx};
use photoedit_core::{face, FaceShape};
use serde_json::{json, Value};
use stitch_core::Alpha;

pub fn cached_faces(ctx: &Ctx, img: &Image, w: usize, h: usize) -> Result<Vec<face::FaceBox>> {
    Ok(radj::faces(ctx, img.id)?.iter().filter_map(|r| {
        let b: [f32;4] = serde_json::from_str(r.get("box")?.as_str()?).ok()?;
        let points: [[f32;2];5] = face::parse_points(r.get("landmarks")?.as_str()?).try_into().ok()?;
        Some(face::FaceBox { x:b[0]*w as f32,y:b[1]*h as f32,w:b[2]*w as f32,h:b[3]*h as f32, score:r.get("score")?.as_f64()? as f32, points: points.map(|p| [p[0]*w as f32,p[1]*h as f32]) })
    }).collect())
}
pub fn cached_shape(ctx: &Ctx, img: &Image) -> Option<FaceShape> {
    // Never merge different faces into one point cloud.
    let rows = radj::landmarks(ctx, img.id).ok()?;
    let row = rows.iter().find(|r| r["idx"].as_i64() == Some(0))?;
    face::shape_from_mesh(&face::parse_points(row["points"].as_str()?))
}
pub fn has_landmarks(ctx: &Ctx, img: &Image) -> bool { cached_shape(ctx, img).is_some() }
pub fn cached_analysis(ctx: &Ctx, img: &Image) -> Result<Value> {
    let faces = cached_faces(ctx, img, img.w.max(1) as usize, img.h.max(1) as usize)?;
    let suggestions: Vec<_> = faces.first().map(|f| face::RATIOS.iter().filter(|(_,r)| *r > 0.0).map(|(label,r)| json!({"ratio":label,"crop":face::crop_suggestion(img.w.max(1) as usize,img.h.max(1) as usize,f,*r)})).collect()).unwrap_or_default();
    Ok(json!({"analyzed": !radj::landmarks(ctx,img.id)?.is_empty(), "count":faces.len(), "has_landmarks":has_landmarks(ctx,img),"suggestions":suggestions}))
}
pub fn auto_mask(ctx: &Ctx, img: &Image, w: usize, h: usize) -> Result<Alpha> {
    let groups: Vec<Vec<[f32;2]>> = radj::landmarks(ctx,img.id)?.iter().filter_map(|r| r["points"].as_str().map(face::parse_points)).collect();
    Ok(face::mesh_beauty_region(w,h,&groups))
}

pub fn crop(ctx: &Ctx, img: &Image, geometry: &photoedit_core::ops::Geometry, ratio: f32) -> Result<Value> {
    let (w,h)=(img.w.max(1) as usize,img.h.max(1) as usize);
    let faces=cached_faces(ctx,img,w,h)?;
    let f=faces.first().ok_or_else(|| crate::error::AppError::bad("srv.face.noFace"))?;
    let mut g=geometry.clone(); g.crop=None;
    let (nw,nh)=photoedit_core::geometry::out_size(w,h,&g);
    let corners=[[f.x,f.y],[f.x+f.w,f.y],[f.x,f.y+f.h],[f.x+f.w,f.y+f.h]];
    let mapped=corners.map(|p| photoedit_core::geometry::map_point([p[0]/w as f32,p[1]/h as f32],w,h,&g));
    let (mut lo,mut hi)=([f32::MAX;2],[f32::MIN;2]);
    for p in mapped { for a in 0..2 { lo[a]=lo[a].min(p[a]); hi[a]=hi[a].max(p[a]); }}
    let mut f=*f; f.x=lo[0]*nw as f32; f.y=lo[1]*nh as f32;
    f.w=(hi[0]-lo[0])*nw as f32; f.h=(hi[1]-lo[1])*nh as f32;
    Ok(json!({"crop":face::crop_suggestion(nw,nh,&f,ratio)}))
}
