//! 关键点限定的多人精修与人体区域变形。先修颜色再变形，五官蒙版随像素一起移动。
use crate::{beauty, face, ops::Beauty, portrait_ops::{Body, FaceEdit, Mesh, Portrait}, px::{sample, Pad}, warp};
use px_core::{Alpha, Rgba};

const INNER_LIPS: &[usize] = &[78,95,88,178,87,14,317,402,318,324,308,415,310,311,312,13,82,81,80,191];
const LIPS: &[usize] = &[61,146,91,181,84,17,314,405,321,375,291,409,270,269,267,0,37,39,40,185];
const BROW_L: &[usize] = &[70,63,105,66,107,55,65,52,53,46];
const BROW_R: &[usize] = &[300,293,334,296,336,285,295,282,283,276];

fn inside(p: [f32;2], poly: &[[f32;2]]) -> bool {
    let mut yes = false;
    for i in 0..poly.len() {
        let (a,b) = (poly[i], poly[(i+1)%poly.len()]);
        if (a[1]>p[1]) != (b[1]>p[1]) && p[0] < (b[0]-a[0])*(p[1]-a[1])/(b[1]-a[1])+a[0] { yes = !yes; }
    }
    yes
}
fn polygon(points: &[[f32;2]], ids: &[usize], w: usize, h: usize) -> Vec<[f32;2]> {
    ids.iter().map(|&i| [points[i][0]*w as f32,points[i][1]*h as f32]).collect()
}
fn distance(p: [f32;2], poly: &[[f32;2]]) -> f32 {
    let mut best = f32::MAX;
    for i in 0..poly.len() {
        let (a,b) = (poly[i],poly[(i+1)%poly.len()]);
        let d = [b[0]-a[0],b[1]-a[1]];
        let t = (((p[0]-a[0])*d[0]+(p[1]-a[1])*d[1])/(d[0]*d[0]+d[1]*d[1]).max(1e-6)).clamp(0.0,1.0);
        best = best.min((p[0]-a[0]-t*d[0]).powi(2)+(p[1]-a[1]-t*d[1]).powi(2));
    }
    best.sqrt()
}
fn weight(p: [f32;2], poly: &[[f32;2]], feather: f32) -> f32 {
    if !inside(p,poly) { return 0.0; }
    let t = (distance(p,poly)/feather.max(1.0)).clamp(0.0,1.0);
    t*t*(3.0-2.0*t)
}
fn bounded_weight(p:[f32;2],poly:&[[f32;2]],bounds:(usize,usize,usize,usize),feather:f32)->f32 {
    let (x0,y0,x1,y1)=bounds;
    if p[0]<x0 as f32||p[0]>=x1 as f32||p[1]<y0 as f32||p[1]>=y1 as f32 {0.0} else {weight(p,poly,feather)}
}
fn oval_mask(w: usize, h: usize, mesh: &Mesh) -> Alpha {
    if w.max(h)>1600 {
        let scale=1600.0/w.max(h) as f32;
        let small=oval_mask((w as f32*scale).round().max(1.0) as usize,(h as f32*scale).round().max(1.0) as usize,mesh);
        return px_core::resize_alpha(&small,w,h);
    }
    let poly = polygon(&mesh.points, face::OVAL, w, h);
    let mut m = Alpha::new(w,h);
    let feather = (face::shape_from_mesh(&mesh.points).unwrap().box_norm[2]*w as f32*0.025).max(1.0);
    let (x0,y0,x1,y1) = bounds(&poly,w,h);
    for y in y0..y1 { for x in x0..x1 { m.set(x,y,(weight([x as f32,y as f32],&poly,feather)*255.0).round() as u8); }}
    m
}
fn skin_mask(w: usize,h: usize,mesh:&Mesh,protect:bool)->Alpha {
    if !protect {return oval_mask(w,h,mesh);}
    if w.max(h)>1600 {
        let scale=1600.0/w.max(h) as f32;
        let small=skin_mask((w as f32*scale).round().max(1.0) as usize,(h as f32*scale).round().max(1.0) as usize,mesh,protect);
        return px_core::resize_alpha(&small,w,h);
    }
    let mut skin=face::mesh_beauty_region(w,h,&[mesh.points.clone()]);
    let brows=[polygon(&mesh.points,BROW_L,w,h),polygon(&mesh.points,BROW_R,w,h)];
    let fw=face::shape_from_mesh(&mesh.points).unwrap().box_norm[2]*w as f32;
    let oval=polygon(&mesh.points,face::OVAL,w,h);
    let (x0,y0,x1,y1)=bounds(&oval,w,h);
    for y in y0..y1 {for x in x0..x1 {
        let p=[x as f32,y as f32];
        if brows.iter().any(|b|inside(p,b)||distance(p,b)<fw*0.018)||distance(p,&oval)<fw*0.02 {skin.set(x,y,0);}
    }}
    skin
}
fn bounds(poly: &[[f32;2]], w: usize, h: usize) -> (usize,usize,usize,usize) {
    let axis = |a: usize| {
        (poly.iter().map(|p| p[a]).fold(f32::MAX,f32::min).floor().clamp(0.0,[w,h][a] as f32) as usize,
         poly.iter().map(|p| p[a]).fold(f32::MIN,f32::max).ceil().clamp(0.0,[w,h][a] as f32) as usize)
    };
    let (x0,x1)=axis(0); let (y0,y1)=axis(1); (x0,y0,x1,y1)
}

/// 精修必须有完整的密集点，点丢失时只跳过对应脸，不能回退为整图处理。
pub fn apply(img: &Rgba, ops: &Portrait, meshes: &[Mesh], painted: Option<&Alpha>) -> Rgba {
    let mut cur = img.clone();
    for edit in ops.faces.iter().filter(|f| !f.is_empty()) {
        let Some(mesh) = meshes.iter().find(|m| m.idx == edit.idx && face::shape_from_mesh(&m.points).is_some()) else { continue; };
        let shape = face::shape_from_mesh(&mesh.points).unwrap();
        // 皮肤保护遮罩在有限档位栅格化，避免大图逐像素扫轮廓线段。
        let mut skin = skin_mask(img.w,img.h,mesh,edit.protect_features);
        if edit.beauty.by_mask {
            if let Some(m) = painted.filter(|m| m.w==img.w && m.h==img.h) {
                for (a,b) in skin.v.iter_mut().zip(&m.v) { *a=((*a as u16 * *b as u16)/255) as u8; }
            } else { skin.v.fill(0); }
        }
        if !edit.beauty.is_empty() {
            cur = beauty::apply(&cur,&Beauty { auto_face:false,..edit.beauty.clone() },Some(&skin));
        }
        if !edit.retouch.is_empty() || !edit.makeup.is_empty() { retouch(&mut cur, edit, mesh, &skin); }
        if !edit.auto.is_empty() || !edit.shape.is_empty() {
            let (p,q)=shape_pairs(edit,mesh,img.w,img.h);
            let protection = edit.protect_background.then(|| oval_mask(img.w,img.h,mesh));
            cur = warp::mls_pairs(&cur,&shape,&p,&q,protection.as_ref());
        }
    }
    if !ops.body.strength.is_empty() { cur = body(&cur,&ops.body); }
    cur
}

/// 自动皮肤遮罩复用与照片相同的形变，不能再按塑形前的位置磨皮。
pub(crate) fn deform_mask(mask: &Alpha, ops: &Portrait, meshes: &[Mesh]) -> Alpha {
    let mut img=Rgba::new(mask.w,mask.h);
    for (i,v) in mask.v.iter().enumerate() {img.px[i*4+3]=*v;}
    for edit in &ops.faces {
        if edit.auto.is_empty() && edit.shape.is_empty() {continue;}
        let Some(mesh)=meshes.iter().find(|m| m.idx==edit.idx && face::shape_from_mesh(&m.points).is_some()) else {continue;};
        let shape=face::shape_from_mesh(&mesh.points).unwrap();
        let (p,q)=shape_pairs(edit,mesh,img.w,img.h);
        let protection=edit.protect_background.then(|| oval_mask(img.w,img.h,mesh));
        img=warp::mls_pairs(&img,&shape,&p,&q,protection.as_ref());
    }
    if !ops.body.strength.is_empty() {img=body(&img,&ops.body);}
    img.alpha()
}

fn retouch(img: &mut Rgba, edit: &FaceEdit, mesh: &Mesh, skin: &Alpha) {
    let p = &mesh.points;
    let shape = face::shape_from_mesh(p).unwrap();
    let (w,h)=(img.w,img.h);
    let px = |i: usize| [p[i][0]*w as f32,p[i][1]*h as f32];
    let oval=polygon(p,face::OVAL,w,h);
    let lips=polygon(p,LIPS,w,h); let inner=polygon(p,INNER_LIPS,w,h);
    let brows=[polygon(p,BROW_L,w,h),polygon(p,BROW_R,w,h)];
    let eyes=[polygon(p,&[33,160,158,133,153,145],w,h),polygon(p,&[263,387,385,362,380,374],w,h)];
    let lip_bounds=bounds(&lips,w,h);let inner_bounds=bounds(&inner,w,h);
    let eye_bounds=eyes.each_ref().map(|v|bounds(v,w,h));let brow_bounds=brows.each_ref().map(|v|bounds(v,w,h));
    let fw=(shape.box_norm[2]*w as f32).max(1.0);
    let mut axis=[px(263)[0]-px(33)[0],px(263)[1]-px(33)[1]];
    let len=(axis[0]*axis[0]+axis[1]*axis[1]).sqrt().max(1.0); axis=[axis[0]/len,axis[1]/len];
    let mut down=[-axis[1],axis[0]];
    if (px(152)[0]-px(1)[0])*down[0]+(px(152)[1]-px(1)[1])*down[1]<0.0 { down=[-down[0],-down[1]]; }
    let eye_centers=[shape.iris_l,shape.iris_r].map(|v| [v[0]*w as f32,v[1]*h as f32]);
    let ellipse = |pos:[f32;2], center:[f32;2], rx:f32, ry:f32| {
        let d=[pos[0]-center[0],pos[1]-center[1]];
        let r=((d[0]*axis[0]+d[1]*axis[1])/rx).powi(2)+((d[0]*down[0]+d[1]*down[1])/ry).powi(2);
        (1.0-r).max(0.0).powi(2)
    };
    let (x0,y0,x1,y1)=bounds(&oval,w,h);
    let r=&edit.retouch; let m=&edit.makeup;
    for y in y0..y1 { for x in x0..x1 {
        let pos=[x as f32,y as f32]; let i=(y*w+x)*4;
        let src=[img.px[i] as f32,img.px[i+1] as f32,img.px[i+2] as f32];
        let mut rgb=src; let sk=skin.get(x,y) as f32/255.0;
        let luminance=src[0]*0.299+src[1]*0.587+src[2]*0.114;
        let mut under=0.0f32; let mut bag=0.0f32; let mut cheek=0.0f32;
        for c in eye_centers {
            under=under.max(ellipse(pos,[c[0]+down[0]*fw*0.10,c[1]+down[1]*fw*0.10],fw*0.15,fw*0.065));
            bag=bag.max(ellipse(pos,[c[0]+down[0]*fw*0.15,c[1]+down[1]*fw*0.15],fw*0.14,fw*0.045));
            cheek=cheek.max(ellipse(pos,[c[0]+down[0]*fw*0.29,c[1]+down[1]*fw*0.29],fw*0.16,fw*0.10));
        }
        let lift=(under*r.dark_circles.pos()*18.0*(1.0-luminance/255.0)+bag*r.eye_bags.pos()*9.0)*sk;
        for v in &mut rgb { *v+=lift; }
        // 色度增量近似保持亮度，红润、去红与去黄互相独立。
        let rose=r.rosy.pos()*sk*6.0;
        let red=r.redness.pos()*sk*(src[0]-src[1]).max(0.0)*0.25;
        let yellow=r.yellow.pos()*sk*((src[0]+src[1])*0.5-src[2]).max(0.0)*0.20;
        rgb[0]+=rose-red-yellow*0.3; rgb[1]+=-rose*0.45+red*0.5-yellow*0.2; rgb[2]+=yellow+red*0.25;
        let ew=if r.eye_light.is_zero() {0.0} else {eyes.iter().zip(eye_bounds).map(|(v,b)|bounded_weight(pos,v,b,fw*0.01)).fold(0.0f32,f32::max)};
        let neutral=((src.iter().copied().fold(f32::MIN,f32::max)-src.iter().copied().fold(f32::MAX,f32::min))/80.0).clamp(0.0,1.0);
        let eye=ew*r.eye_light.pos()*(1.0-neutral)*(luminance/160.0).clamp(0.0,1.0)*14.0;
        let tooth=if r.teeth.is_zero() {0.0} else {bounded_weight(pos,&inner,inner_bounds,fw*0.012)*r.teeth.pos()*(1.0-neutral)*((luminance-65.0)/90.0).clamp(0.0,1.0)};
        for c in 0..3 { rgb[c]+=eye+tooth*((luminance-rgb[c])*0.45+(255.0-luminance)*0.18); }
        let lip=if m.lipstick.is_zero()&&m.gloss.is_zero() {0.0} else {(bounded_weight(pos,&lips,lip_bounds,fw*0.012)-bounded_weight(pos,&inner,inner_bounds,fw*0.008)).max(0.0)};
        let tint = |rgb:&mut [f32;3], color:[u8;3], k:f32| {
            // 颜色沿原明暗调制，唇纹与面部阴影仍然保留。
            for c in 0..3 { rgb[c]+=(color[c] as f32*(0.55+luminance/360.0)-rgb[c])*k; }
        };
        tint(&mut rgb,edit.lip_color,lip*m.lipstick.pos()*0.65);
        tint(&mut rgb,edit.blush_color,cheek*sk*m.blush.pos()*0.25);
        let brow=if m.brows.is_zero() {0.0} else {brows.iter().zip(brow_bounds).map(|(b,bb)|bounded_weight(pos,b,bb,fw*0.008)).fold(0.0f32,f32::max)*m.brows.pos()*0.30};
        for v in &mut rgb { *v*=1.0-brow; *v+=lip*m.gloss.pos()*((luminance-100.0)/155.0).clamp(0.0,1.0)*16.0; }
        for c in 0..3 { img.px[i+c]=rgb[c].round().clamp(0.0,255.0) as u8; }
    }}
}

fn shape_pairs(edit: &FaceEdit, mesh: &Mesh, w: usize, h: usize) -> (Vec<(f32,f32)>,Vec<(f32,f32)>) {
    let shape=face::shape_from_mesh(&mesh.points).unwrap();
    let (mut p,mut q)=warp::control_pairs(&edit.auto,&shape,w,h);
    let px=|i:usize| (mesh.points[i][0]*w as f32,mesh.points[i][1]*h as f32);
    let (l,r)=(px(33),px(263));
    let fw=((px(454).0-px(234).0).powi(2)+(px(454).1-px(234).1).powi(2)).sqrt().max(1.0);
    let d=((r.0-l.0).powi(2)+(r.1-l.1).powi(2)).sqrt().max(1.0);
    let u=((r.0-l.0)/d,(r.1-l.1)/d);
    let mut v=(-u.1,u.0); let chin=px(152); let nose=px(1);
    if (chin.0-nose.0)*v.0+(chin.1-nose.1)*v.1<0.0 { v=(-v.0,-v.1); }
    let s=&edit.shape;
    let mut add=|i:usize,dx:f32,dy:f32| {
        if dx.abs()+dy.abs()<1e-5 { return; }
        let a=px(i); let b=(a.0+u.0*dx+v.0*dy,a.1+u.1*dx+v.1*dy);
        if let Some(j)=p.iter().position(|z| (z.0-a.0).abs()<0.1&&(z.1-a.1).abs()<0.1) { q[j].0+=b.0-a.0; q[j].1+=b.1-a.1; }
        else { p.push(a); q.push(b); }
    };
    for (i,sign) in [(234,1.0),(93,1.0),(454,-1.0),(323,-1.0)] { add(i,sign*s.cheekbone.norm()*fw*0.055,0.0); }
    for (i,sign) in [(172,1.0),(136,1.0),(397,-1.0),(365,-1.0)] { add(i,sign*s.jaw_width.norm()*fw*0.065,0.0); }
    for &i in face::OVAL {
        let a=px(i); let cx=(px(234).0+px(454).0)*0.5; let cy=(px(234).1+px(454).1)*0.5;
        let horiz=(a.0-cx)*u.0+(a.1-cy)*u.1;
        let vert=(a.0-nose.0)*v.0+(a.1-nose.1)*v.1;
        add(i,-horiz*s.face_width.norm()*0.12,vert*s.face_length.norm()*0.10);
    }
    for i in [152,148,377] { add(i,0.0,s.chin_length.norm()*fw*0.08); }
    for (ring,center,sign) in [(&shape.eye_l,shape.iris_l,-1.0),(&shape.eye_r,shape.iris_r,1.0)] {
        for &pt in ring {
            let Some(i)=mesh.points.iter().position(|p| *p==pt) else { continue; };
            let a=px(i); let cx=center[0]*w as f32; let cy=center[1]*h as f32;
            let rx=a.0-cx; let ry=a.1-cy; let angle=s.eye_tilt.norm()*0.12*sign;
            let (sn,cs)=angle.sin_cos(); let dx=rx*cs-ry*sn-rx; let dy=rx*sn+ry*cs-ry;
            add(i,sign*s.eye_distance.norm()*fw*0.035+dx*u.0+dy*u.1,-s.eye_height.norm()*fw*0.03+dx*v.0+dy*v.1);
        }
    }
    for i in [1,2,4,5] { let a=px(i); let d=(nose.0-a.0,nose.1-a.1); add(i,(d.0*u.0+d.1*u.1)*s.nose_tip.norm()*0.20,(d.0*v.0+d.1*v.1)*s.nose_tip.norm()*0.20+s.nose_length.norm()*fw*0.045); }
    let mouth=px(13);
    for &i in LIPS {
        let a=px(i); let dx=(a.0-mouth.0)*u.0+(a.1-mouth.1)*u.1;
        let dy=(a.0-mouth.0)*v.0+(a.1-mouth.1)*v.1;
        add(i,dx*s.mouth_width.norm()*0.20,dy*if dy<0.0 { s.upper_lip.norm()*0.35 } else { s.lower_lip.norm()*0.35 });
    }
    if !p.is_empty() {
        let [x,y,bw,bh]=shape.box_norm;
        for a in [[x-bw*0.1,y-bh*0.1],[x+bw*1.1,y-bh*0.1],[x-bw*0.1,y+bh*1.1],[x+bw*1.1,y+bh*1.1]] {
            let a=(a[0]*w as f32,a[1]*h as f32); p.push(a); q.push(a);
        }
    }
    (p,q)
}

/// 人体框内部的连续反向映射；框边与框外位移为零，肩以上不拉长。
fn body(img: &Rgba, body: &Body) -> Rgba {
    let mut out=img.clone(); let [bx,by,bw,bh]=body.region;
    if bw<=0.0 || bh<=0.0 { return out; }
    let (w,h)=(img.w,img.h);
    let (x0,y0)=((bx*w as f32).clamp(0.0,w as f32) as usize,(by*h as f32).clamp(0.0,h as f32) as usize);
    let (x1,y1)=(((bx+bw)*w as f32).clamp(0.0,w as f32) as usize,((by+bh)*h as f32).clamp(0.0,h as f32) as usize);
    let s=&body.strength; let [shoulder,waist,hip,knee]=body.levels;
    let bump=|a:f32,c:f32,r:f32| (1.0-((a-c)/r).powi(2)).max(0.0).powi(2);
    for y in y0..y1 { for x in x0..x1 {
        let u=(x as f32/w as f32-bx)/bw; let v=(y as f32/h as f32-by)/bh;
        let edge=(u.min(1.0-u).min(v).min(1.0-v)*10.0).clamp(0.0,1.0);
        let fade=edge*edge*(3.0-2.0*edge);
        let t=(u-0.5)*2.0;
        let horizontal=s.waist.norm()*bump(v,waist,0.13)*t*0.045
            + s.shoulders.norm()*bump(v,shoulder,0.10)*t*0.04
            + s.arms.norm()*bump(v,(shoulder+waist)*0.5,0.20)*bump(t.abs(),0.72,0.23)*t.signum()*0.025
            + s.legs.norm()*bump(v,(hip+knee)*0.5,0.28)*(bump(u,0.32,0.15)*(u-0.32)+bump(u,0.68,0.15)*(u-0.68))*0.35;
        let stretch=s.long_legs.norm()*bump(v,(hip+1.0)*0.5,(1.0-hip)*0.5)*0.035;
        if fade==0.0 || horizontal==0.0 && stretch==0.0 { continue; }
        let c=sample(&img.px,w,h,x as f32+horizontal*bw*w as f32*fade,y as f32-stretch*bh*h as f32*fade,Pad::Edge);
        let i=(y*w+x)*4; for a in 0..4 { out.px[i+a]=c[a].round().clamp(0.0,255.0) as u8; }
    }}
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::Slider;
    pub(crate) fn fixture(idx: usize, cx: f32) -> Mesh {
        let mut points=vec![[cx,0.45];468];
        let mut ring=|ids:&[usize],c:[f32;2],r:[f32;2]| {
            for (j,&i) in ids.iter().enumerate() {
                let angle=-std::f32::consts::FRAC_PI_2+j as f32/ids.len() as f32*std::f32::consts::TAU;
                points[i]=[c[0]+r[0]*angle.cos(),c[1]+r[1]*angle.sin()];
            }
        };
        ring(face::OVAL,[cx,0.48],[0.18,0.32]);
        ring(&[33,7,163,144,145,153,154,155,133,173,157,158,159,160,161,246],[cx-0.075,0.39],[0.045,0.025]);
        ring(&[263,249,390,373,374,380,381,382,362,398,384,385,386,387,388,466],[cx+0.075,0.39],[0.045,0.025]);
        ring(LIPS,[cx,0.64],[0.075,0.038]); ring(INNER_LIPS,[cx,0.64],[0.052,0.018]);
        ring(BROW_L,[cx-0.075,0.32],[0.052,0.016]);ring(BROW_R,[cx+0.075,0.32],[0.052,0.016]);
        points[98]=[cx-0.035,0.51];points[327]=[cx+0.035,0.51];
        for i in [1,2,4,5] { points[i]=[cx+(i as f32-3.0)*0.004,0.51+(i as f32-3.0)*0.009]; }
        Mesh{idx,points}
    }
    fn image() -> Rgba {
        let mut img=Rgba::new(160,140);
        for y in 0..img.h { for x in 0..img.w { img.set(x,y,[140+(x%70) as u8,115+(y%70) as u8,100+(x%50) as u8,197]); }} img
    }
    #[test]
    fn every_shape_slider_moves_controls_and_supports_both_directions() {
        let mesh=fixture(0,0.5);
        for key in ["cheekbone","jaw_width","face_width","face_length","chin_length","eye_distance","eye_height","eye_tilt","nose_tip","nose_length","mouth_width","upper_lip","lower_lip"] {
            let edit:FaceEdit=serde_json::from_value(serde_json::json!({"shape":{key:70}})).unwrap();
            let neg:FaceEdit=serde_json::from_value(serde_json::json!({"shape":{key:-70}})).unwrap();
            let (p,q)=shape_pairs(&edit,&mesh,160,140);let (np,nq)=shape_pairs(&neg,&mesh,160,140);
            assert!(p.iter().zip(&q).any(|(a,b)| (a.0-b.0).abs()+(a.1-b.1).abs()>0.1),"{key}");
            assert_eq!(p,np);assert_ne!(q,nq,"{key}");
            let img=image();let o=Portrait{faces:vec![edit],..Default::default()};
            assert_ne!(apply(&img,&o,&[mesh.clone()],None),img,"{key} pixels");
        }
    }
    #[test]
    fn selected_face_does_not_touch_other_face_or_background() {
        let meshes=[fixture(0,0.25),fixture(1,0.75)]; let img=image();
        let edit:FaceEdit=serde_json::from_value(serde_json::json!({"idx":1,"auto":{"face_slim":40},"retouch":{"rosy":70},"makeup":{"lipstick":70}})).unwrap();
        let o=Portrait{faces:vec![edit],..Default::default()};let out=apply(&img,&o,&meshes,None);
        assert_ne!(out,img);
        for y in 0..140 { for x in 0..80 {assert_eq!(out.get(x,y),img.get(x,y));}}
        assert_eq!(out.get(159,139),img.get(159,139));
        assert!(out.px.chunks_exact(4).all(|p|p[3]==197));
        let sparse=[Mesh{idx:0,points:Vec::new()},meshes[1].clone()];
        assert_eq!(apply(&img,&o,&sparse,None),out,"missing main face must not shift the selected face index");
    }
    #[test]
    fn closed_mouth_does_not_whiten_lips() {
        let img=image();let mut mesh=fixture(0,0.5);
        for &i in INNER_LIPS {mesh.points[i]=[0.5,0.64];}
        let mut f=FaceEdit::default();f.retouch.teeth=Slider(100);
        assert_eq!(apply(&img,&Portrait{faces:vec![f],..Default::default()},&[mesh],None),img);
    }
    #[test]
    fn local_retouch_and_makeup_are_effective_and_preserve_exterior() {
        let mesh=fixture(0,0.5);
        for (group,keys) in [("retouch",vec!["dark_circles","eye_bags","teeth","eye_light","rosy","redness","yellow"]),("makeup",vec!["blush","lipstick","brows","gloss"])] {
            for key in keys {
                let mut img=image();
                // 牙齿与眼白采用非饱和亮色，验证颜色门控后的真实效果。
                if key=="teeth"||key=="eye_light" {for p in img.px.chunks_exact_mut(4){p[..3].copy_from_slice(&[190,182,163]);}}
                let f:FaceEdit=serde_json::from_value(serde_json::json!({group:{key:100}})).unwrap();
                let o=Portrait{faces:vec![f],..Default::default()};let out=apply(&img,&o,&[mesh.clone()],None);
                assert_ne!(out,img,"{key}");assert_eq!(out.get(0,0),img.get(0,0));assert_eq!(out.get(159,139),img.get(159,139));
                if key=="teeth" { assert_eq!(out.get(80,84),img.get(80,84),"outer lip must stay unchanged"); }
            }
        }
    }
    #[test]
    fn empty_painted_mask_disables_face_skin_and_skin_retouch() {
        let img=image();let mesh=fixture(0,0.5);
        let f:FaceEdit=serde_json::from_value(serde_json::json!({"beauty":{"smooth":70,"by_mask":true},"retouch":{"rosy":80}})).unwrap();
        let o=Portrait{faces:vec![f],..Default::default()};assert_eq!(apply(&img,&o,&[mesh],None),img);
    }
    #[test]
    fn missing_faces_never_apply_retouch_to_background() {
        let mut img=Rgba::new(40,60); img.px.fill(160);
        let mut f=FaceEdit::default(); f.retouch.teeth=Slider(100); f.makeup.lipstick=Slider(100);
        let o=Portrait { faces:vec![f], ..Default::default() };
        assert_eq!(apply(&img,&o,&[],None),img);
    }
    #[test]
    fn body_preserves_region_exterior_and_changes_inside() {
        let mut img=Rgba::new(120,180);
        for y in 0..180 { for x in 0..120 { img.set(x,y,[(x*2) as u8,y as u8,80,255]); }}
        for name in 0..5 {
            let mut b=Body::default();
            match name { 0=>b.strength.waist=Slider(100),1=>b.strength.shoulders=Slider(100),2=>b.strength.arms=Slider(100),3=>b.strength.legs=Slider(100),_=>b.strength.long_legs=Slider(100) }
            let out=body(&img,&b); assert_ne!(out,img,"body control {name}");
            for y in 0..180 { for x in 0..120 { if x<24 || x>=96 || y<9 || y>=171 { assert_eq!(out.get(x,y),img.get(x,y)); } }}
        }
    }
}
