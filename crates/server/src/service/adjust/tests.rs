#![cfg(test)]
//! 模块行为与边界回归。
use super::*;
use crate::repo::{db, images as rimg};
use crate::state::Ctx;

/// 一个隔离的小库：临时目录 + 一张真的 PNG。测试绝不碰用户的 data/
fn fixture(name: &str) -> (std::path::PathBuf, std::sync::Arc<Ctx>, Image) {
    let dir = std::env::temp_dir().join(format!("synco-adj-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = Ctx::new(dir.clone(), dir.clone(), db::open(&dir).unwrap());
    crate::repo::projects::create(&ctx, "测试项目").unwrap();
    // 一张 40×20 的渐变图：够小、又不退化（左右两半颜色不同，调色后能有可见差别）
    let mut img = Rgba::new(40, 20);
    for y in 0..20 {
        for x in 0..40 {
            img.set(x, y, [(x * 6) as u8, (y * 12) as u8, 128, 255]);
        }
    }
    let rel = util::rel_path(&["projects".into(), "1".into(), "p.png".into()]);
    std::fs::create_dir_all(dir.join("projects").join("1")).unwrap();
    std::fs::write(dir.join(&rel), codec::encode_png(&img)).unwrap();
    let id = rimg::insert(&ctx, 1, "p.png", &rel, 40, 20).unwrap();
    let row = rimg::by_id(&ctx, id).unwrap().unwrap();
    (dir, ctx, row)
}

/// 用完就连目录一起删掉：连接还开着的时候 Windows 不让删，所以先把 ctx 交回来再丢
fn cleanup(ctx: std::sync::Arc<Ctx>, dir: &std::path::PathBuf) {
    drop(ctx);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn development_snapshot_replays_curves_local_masks_and_seeded_effects() {
    let (dir,ctx,img)=fixture("development-snapshot");
    let (ops,_)=EditOps::parse(r#"{"geometry":{"rotate_deg":90,"flip_h":true},"develop":{"look":{"hsl":[{},{"hue":15,"saturation":20},{},{},{},{},{},{}],"curves":{"red":[[0,0],[0.5,0.6],[1,1]]},"grading":{"shadows":{"hue":220,"saturation":20}}},"detail":{"chroma":35},"effects":{"grain":45,"seed":142,"vignette":-20},"locals":[{"mask":{"kind":"brush","strokes":[{"points":[[0.2,0.3],[0.6,0.5]],"radius":0.12}]},"color":{"exposure":35}}]}}"#).unwrap();
    let expected=photo_with(&ctx,&img,&ops).unwrap();let snap=snapshot_inputs(&ctx,&img,&ops).unwrap();
    radj::put(&ctx,img.id,&EditOps::default().to_json()).unwrap();
    let restored=photo_from_snapshot(&ctx,&img,&ops,&snap,None).unwrap();assert_eq!(expected,restored);
    assert_ne!(restored,photoedit_core::apply_geometry(&orig_rgba(&ctx,&img).unwrap(),&ops.geometry));
    cleanup(ctx,&dir);
}

#[test]
fn portrait_snapshot_preserves_each_face_and_painted_mask_after_refresh() {
    let (dir,ctx,mut img)=fixture("portrait-snapshot");
    let mut source=Rgba::new(160,140);
    for y in 0..140 {for x in 0..160 {source.set(x,y,[150+(x%60) as u8,120+(y%50) as u8,110,255]);}}
    std::fs::write(dir.join(&img.orig_path),codec::encode_png(&source)).unwrap();img.w=160;img.h=140;
    let make_mesh=|cx:f32| {
        let mut points=vec![[cx,0.48];468];
        let mut ring=|ids:&[usize],c:[f32;2],r:[f32;2]| {for (j,&i) in ids.iter().enumerate() {let a=-std::f32::consts::FRAC_PI_2+j as f32/ids.len() as f32*std::f32::consts::TAU;points[i]=[c[0]+r[0]*a.cos(),c[1]+r[1]*a.sin()];}};
        ring(photoedit_core::face::OVAL,[cx,0.48],[0.18,0.32]);
        ring(&[33,7,163,144,145,153,154,155,133,173,157,158,159,160,161,246],[cx-0.075,0.39],[0.045,0.025]);
        ring(&[263,249,390,373,374,380,381,382,362,398,384,385,386,387,388,466],[cx+0.075,0.39],[0.045,0.025]);
        points[98]=[cx-0.035,0.51];points[327]=[cx+0.035,0.51];points
    };
    radj::put_analysis(&ctx,img.id,&[],&[json!({"points":make_mesh(0.25)}),json!({"points":make_mesh(0.75)})]).unwrap();
    let mut mask=Rgba::new(160,140);for y in 0..140 {for x in 80..160 {mask.set(x,y,[255;4]);}}
    let mask_png=codec::encode_png(&mask);let rel="projects/1/portrait-mask.png";
    std::fs::write(dir.join(rel),&mask_png).unwrap();img.mask_path=Some(rel.into());
    let (ops,_)=EditOps::parse(r#"{"portrait":{"faces":[{"idx":0,"shape":{"face_width":30}},{"idx":1,"beauty":{"smooth":35,"by_mask":true},"retouch":{"rosy":70}}],"body":{"strength":{"waist":35}}},"geometry":{"rotate_deg":90,"flip_h":true}}"#).unwrap();
    let expected=photo_with(&ctx,&img,&ops).unwrap();assert_ne!(expected,photoedit_core::apply_geometry(&source,&ops.geometry));
    let before=render_hash(&ctx,&img,&ops).unwrap();let snapshot=snapshot_inputs(&ctx,&img,&ops).unwrap();
    radj::put_analysis(&ctx,img.id,&[],&[json!({"points":[]})]).unwrap();
    std::fs::write(dir.join(rel),codec::encode_png(&Rgba::new(160,140))).unwrap();
    assert_ne!(render_hash(&ctx,&img,&ops).unwrap(),before);
    assert_eq!(photo_from_snapshot(&ctx,&img,&ops,&snapshot,Some(&mask_png)).unwrap(),expected);
    assert!(photo_from_snapshot(&ctx,&img,&ops,&json!({"shape":null,"lut":null}),Some(&mask_png)).is_err());
    drop(ctx);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn 调整快照在蒙版和lut改动后仍逐位相同() {
    let (dir, ctx, mut img) = fixture("frozeninputs");
    let lut_dir = dir.join("luts");
    std::fs::create_dir_all(&lut_dir).unwrap();
    let lut_path = lut_dir.join("frozen.cube");
    std::fs::write(&lut_path, "LUT_1D_SIZE 2\n1 1 1\n0 0 0\n").unwrap();
    let mask_rel = "projects/1/mask.png";
    let mut m = Rgba::new(20, 10);
    for y in 0..10 { for x in 0..10 { m.set(x, y, [255, 255, 255, 255]); } }
    let mask = codec::encode_png(&m);
    std::fs::write(dir.join(mask_rel), &mask).unwrap();
    img.mask_path = Some(mask_rel.into());
    let mut ops = EditOps::default();
    ops.lut = Some(photoedit_core::LutRef { name: "frozen.cube".into(), strength: photoedit_core::Slider(100) });
    ops.beauty.by_mask = true;
    ops.beauty.brighten = photoedit_core::Slider(50);
    let want = photo_with(&ctx, &img, &ops).unwrap();
    let inputs = snapshot_inputs(&ctx, &img, &ops).unwrap();
    // 覆盖 LUT 与蒙版之后按原快照重放，与提交时实际计算结果逐位比较。
    std::fs::write(&lut_path, "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap();
    std::fs::write(dir.join(mask_rel), codec::encode_png(&Rgba::new(20, 10))).unwrap();
    let got = photo_from_snapshot(&ctx, &img, &ops, &inputs, Some(&mask)).unwrap();
    assert_eq!(want.px, got.px);
    assert_ne!(want.px, photo_with(&ctx, &img, &ops).unwrap().px);
    cleanup(ctx, &dir);
}

#[test]
fn 参数指纹随内容变随顺序不变() {
    let a = ops_hash(r#"{"v":1,"color":{"exposure":10}}"#);
    let b = ops_hash(r#"{"v":1,"color":{"exposure":10}}"#);
    let c = ops_hash(r#"{"v":1,"color":{"exposure":11}}"#);
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(a.len(), 8);
}

#[test]
fn 存参数读回来是夹逼后的那一份() {
    let (dir, ctx, img) = fixture("save");
    let (mut ops, mut rep) = EditOps::parse(r#"{"color":{"exposure":999,"contrast":-30},"beauty":{"smooth":40,"by_mask":true}}"#).unwrap();
    rep.extend(normalize(&mut ops, 40));
    assert!(rep.iter().any(|x| x == "color.exposure"), "{rep:?}");
    radj::put(&ctx, img.id, &ops.to_json()).unwrap();
    let back = load_ops(&ctx, img.id);
    assert_eq!(back.color.exposure.0, 100);
    assert_eq!(back.color.contrast.0, -30);
    assert!(back.beauty.by_mask);
    assert!(!back.is_identity());
    cleanup(ctx, &dir);
}

#[test]
fn 坏参数退全默认而不是把图卡死() {
    let (dir, ctx, img) = fixture("badops");
    radj::put(&ctx, img.id, "{不是 JSON").unwrap();
    assert!(load_ops(&ctx, img.id).is_identity());
    cleanup(ctx, &dir);
}

#[test]
fn 轨迹入库前抽稀() {
    let (dir, ctx, _img) = fixture("simplify");
    let mut ops = EditOps::default();
    // 一条直线上的 31 个点：容差 1px（图宽 40 → 归一化 1/40）该压成两端
    ops.warp.strokes = vec![photoedit_core::Stroke {
        tool: photoedit_core::Tool::Push,
        points: (0..31).map(|i| [i as f32 / 40.0, i as f32 / 40.0]).collect(),
        radius: 0.1,
        strength: photoedit_core::Slider(70),
    }];
    let rep = normalize(&mut ops, 40);
    assert_eq!(ops.warp.strokes[0].points.len(), 2, "抽稀没生效：{:?}", ops.warp.strokes[0].points);
    assert!(rep.iter().any(|x| x.starts_with("warp.strokes.points")), "{rep:?}");
    cleanup(ctx, &dir);
}

#[test]
fn histogram_sidecars_follow_preview_revisions_and_recover_missing_cache() {
    let(dir,ctx,img)=fixture("histogram-cache");let mut ops=EditOps::default();ops.color.exposure.0=35;
    let a=build_preview(&ctx,&img,&ops).unwrap();let h=histogram::preview(&ctx,&img,&ops,&a).unwrap();
    assert_eq!(h.pixels,800);assert_eq!(h,photoedit_core::histogram::compute(&pixels(&ctx,&img,&ops,Grade::Proxy).unwrap()));
    assert!(a.abs.with_extension("hist.json").is_file());
    std::fs::write(a.abs.with_extension("hist.json"),b"broken json").unwrap();
    assert_eq!(histogram::preview(&ctx,&img,&ops,&a).unwrap(),h);
    ops.color.exposure.0=-40;let b=build_preview(&ctx,&img,&ops).unwrap();assert!(!a.abs.with_extension("hist.json").exists());
    assert_ne!(histogram::preview(&ctx,&img,&ops,&b).unwrap().red,h.red);
    let source=histogram::source(&ctx,&img).unwrap();assert_eq!(source,photoedit_core::histogram::compute(&orig_rgba(&ctx,&img).unwrap()));
    cleanup(ctx,&dir);
}

#[test]
fn 预览落档并复用() {
    let (dir, ctx, img) = fixture("prev");
    let mut ops = EditOps::default();
    ops.color.exposure = photoedit_core::Slider(40);
    let b = build_preview(&ctx, &img, &ops).unwrap();
    assert!(!b.reused);
    assert!(b.abs.is_file(), "预览档没落盘：{}", b.abs.display());
    assert!(b.rel.contains("adjprev"));
    assert!(b.h > 0, "预览响应缺高度");
    let again = build_preview(&ctx, &img, &ops).unwrap();
    assert!(again.reused, "同参数应当复用而不是重算");
    assert_eq!(again.rel, b.rel);
    // 换个参数就是另一个文件，旧的那份被清掉（目录里只留当前参数）
    ops.color.exposure = photoedit_core::Slider(-40);
    let third = build_preview(&ctx, &img, &ops).unwrap();
    assert_ne!(third.rel, b.rel);
    assert!(!b.abs.exists(), "旧预览档没被请走");
    cleanup(ctx, &dir);
}

#[test]
fn 成图尺寸跟着几何段走() {
    let (dir, ctx, img) = fixture("render");
    let mut ops = EditOps::default();
    ops.geometry.rotate_deg = 90.0;
    ops.color.saturation = photoedit_core::Slider(-100);
    let (b, thumb) = build_render(&ctx, &img, &ops).unwrap();
    assert_eq!((b.w, b.h), (20, 40), "转了 90° 却没换边长");
    assert!(b.abs.is_file());
    assert!(thumb.is_some(), "成图该配一张 320 小档");
    cleanup(ctx, &dir);
}

#[test]
fn 另存为新图走派生谱系且新行不带参数() {
    let (dir, ctx, img) = fixture("fork");
    let mut ops = EditOps::default();
    ops.color.contrast = photoedit_core::Slider(50);
    let (b, _) = build_render(&ctx, &img, &ops).unwrap();
    let new_id = fork_from(&ctx, &img, &b.rel, b.w as i64, b.h as i64).unwrap();
    let kid = rimg::by_id(&ctx, new_id).unwrap().unwrap();
    assert_eq!(kid.derived_from, Some(img.id));
    assert_eq!(kid.derived_result, None, "调整另存不挂结果行");
    assert!(kid.thumb_path.is_none() && kid.proxy_path.is_none(), "新行的派生档由后台补，不该在这里半成品");
    assert!(load_ops(&ctx, new_id).is_identity(), "新图不该继承父图的参数");
    // 父图的成图还在（另存是复制，不是改名）
    assert!(b.abs.is_file());
    cleanup(ctx, &dir);
}

#[test]
fn 没调整时提交字节就是文件字节() {
    let (dir, ctx, img) = fixture("submit0");
    let (bytes, rel) = submit_artifact(&ctx, &img).unwrap();
    assert_eq!(rel, img.orig_path);
    assert_eq!(bytes, std::fs::read(ctx.data.join(&img.orig_path)).unwrap(), "零回归要求逐字节一致");
    cleanup(ctx, &dir);
}

#[test]
fn 调整后提交的是重编码那一张并留在盘上() {
    let (dir, ctx, img) = fixture("submit1");
    let mut ops = EditOps::default();
    ops.color.temp = photoedit_core::Slider(60);
    radj::put(&ctx, img.id, &ops.to_json()).unwrap();
    let (bytes, rel) = submit_artifact(&ctx, &img).unwrap();
    assert_ne!(rel, img.orig_path);
    // 提交记录不进缓存层：它被结果行的 orig_path 指着，而这套参数之后会被改掉，删了就是真丢了
    assert!(rel.starts_with(&format!("projects/{}/", img.project_id)), "提交记录不该离开项目目录：{rel}");
    assert!(rel.to_lowercase().contains(&input_prefix(&img)), "名字没带前缀，删图时那条前缀扫描就捞不到它：{rel}");
    assert!(ctx.data.join(&rel).is_file(), "发出去那张要能在库里指着看");
    assert_eq!(&bytes[1..4], b"PNG", "本机那条要的是 PNG");
    assert!(adjusted(&ctx, &img));
    cleanup(ctx, &dir);
}

#[test]
fn 删图清掉附属行与调整档() {
    let (dir, ctx, img) = fixture("purge");
    let mut ops = EditOps::default();
    ops.beauty.smooth = photoedit_core::Slider(50);
    radj::put(&ctx, img.id, &ops.to_json()).unwrap();
    build_preview(&ctx, &img, &ops).unwrap();
    build_render(&ctx, &img, &ops).unwrap();
    // 提交记录那张不在缓存层（它被结果行指着），删图时得靠前缀扫把它一起带走
    let (_, sent_rel) = submit_artifact(&ctx, &img).unwrap();
    assert!(ctx.data.join(&sent_rel).is_file());
    // 数的是缓存层里那一个目录，而且用生产那套路径构造器：规则再挪一次，这条测试要跟着动，
    // 不该在自己身体里再抄一份路径
    let slot_dir = || slots_dir(&ctx, &img);
    let names = || -> Vec<String> {
        std::fs::read_dir(slot_dir()).unwrap().flatten()
            .map(|e| e.file_name().to_string_lossy().to_string()).collect()
    };
    let files = names();
    assert!(files.len() >= 3, "预览/成图/缩略都该在：{files:?}");
    purge_all(&ctx, &img);
    let left = std::fs::read_dir(slot_dir()).map(|_| names()).unwrap_or_default();
    assert!(left.is_empty(), "调整档没清干净：{left:?}");
    assert!(!ctx.data.join(&sent_rel).is_file(), "projects/ 里那张提交记录还在");
    radj::clear(&ctx, img.id).unwrap();
    assert!(radj::ops_of(&ctx, img.id).unwrap().is_none());
    cleanup(ctx, &dir);
}

#[test]
fn 蒙版限定只作用在涂过的那半边() {
    let (dir, ctx, img) = fixture("mask");
    // 左半涂满、右半空白的蒙版 PNG（与图同尺寸）
    let mut m = Rgba::new(40, 20);
    for y in 0..20 {
        for x in 0..40 {
            let v = if x < 20 { 255u8 } else { 0u8 };
            m.set(x, y, [v, v, v, v]);
        }
    }
    let mrel = util::rel_path(&["projects".into(), "1".into(), "p_mask.png".into()]);
    std::fs::write(ctx.data.join(&mrel), codec::encode_png(&m)).unwrap();
    rimg::set_mask(&ctx, img.id, Some(&mrel)).unwrap();
    // 重新取一次行：img 是 set_mask 之前拿的，mask_path 还带着 None
    let img = rimg::by_id(&ctx, img.id).unwrap().unwrap();
    // 底图换成一片肤色：美白只认肤色域，渐变图上的那些像素本来就不该被提亮，
    // 拿它测"蒙版内有效果"会测到算子的第一道闸门而不是蒙版
    let mut skin = Rgba::new(40, 20);
    for y in 0..20 {
        for x in 0..40 {
            skin.set(x, y, [200, 170, 150, 255]);
        }
    }
    std::fs::write(ctx.data.join(&img.orig_path), codec::encode_png(&skin)).unwrap();
    let src = codec::decode(&std::fs::read(ctx.data.join(&img.orig_path)).unwrap()).unwrap();
    let mut ops = EditOps::default();
    ops.beauty.brighten = photoedit_core::Slider(100);
    ops.beauty.by_mask = true;
    let out = pixels(&ctx, &img, &ops, Grade::Full).unwrap();
    let same = |x: usize| (out.get(x, 10)[1] as i32 - src.get(x, 10)[1] as i32).abs() <= 1;
    let off: Vec<(usize, [u8; 4])> = (22..40).filter(|x| !same(*x)).map(|x| (x, out.get(x, 10))).collect();
    assert!(off.is_empty(), "没涂的那半边被提亮了：{off:?}");
    assert!(!same(4), "涂过的那半边没提亮");
    assert!(out.get(4, 10)[1] as i32 > src.get(4, 10)[1] as i32 + 6, "提亮量不够可见");
    cleanup(ctx, &dir);
}

#[test]
fn 面板带上预设与盘上lut名单() {
    let (dir, ctx, img) = fixture("panel");
    std::fs::create_dir_all(ctx.data.join("luts")).unwrap();
    std::fs::write(ctx.data.join("luts").join("B.cube"), "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap();
    std::fs::write(ctx.data.join("luts").join("A.cube"), "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap();
    std::fs::write(ctx.data.join("luts").join("忽略.txt"), "x").unwrap();
    let v = panel(&ctx, img.id).unwrap();
    assert_eq!(v["luts"].as_array().unwrap().len(), 2, "LUT 名单混进了非 .cube：{v:?}");
    // 名单按小写排序，A 在 B 前
    assert_eq!(v["luts"].as_array().unwrap()[0].as_str().unwrap(), "A.cube");
    let ps = v["presets"].as_array().unwrap();
    assert!(ps.len() >= 8);
    // 数值随面板一起下来：前端摆滑杆读这一份，不再自己抄一张表
    assert!(ps.iter().all(|p| p["name"].as_str().is_some() && p["color"].as_object().is_some()), "有预设没带数值：{ps:?}");
    let clean = ps.iter().find(|p| p["id"].as_str() == Some("clean")).unwrap();
    assert_eq!(clean["color"]["exposure"].as_i64(), Some(6), "预设数值与 color::preset 对不上：{clean}");
    assert!(clean["color"]["preset"].is_null(), "预设自己不该带 preset 字段：{clean}");
    assert_eq!(v["ops"]["v"].as_i64(), Some(1));
    cleanup(ctx, &dir);
}

#[test]
fn lut只在data_luts那一层里找() {
    let (dir, ctx, _img) = fixture("lutpath");
    std::fs::create_dir_all(ctx.data.join("luts")).unwrap();
    for bad in ["../app.db", "luts/x.cube", "/etc/passwd"] {
        let err = lut_table(&ctx, bad).unwrap_err().text();
        assert!(err.contains("不合法") || err.contains("读不到"), "{bad} → {err}");
    }
    // 名字合法但文件不存在：说"读不到"而不是崩
    assert!(lut_table(&ctx, "没有这个.cube").unwrap_err().text().contains("读不到"));
    std::fs::write(ctx.data.join("luts").join("ok.cube"), "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap();
    assert!(lut_table(&ctx, "ok.cube").unwrap().is_some());
    assert!(lut_table(&ctx, "").unwrap().is_none(), "没选 LUT 就是没选，不该报错");
    cleanup(ctx, &dir);
}

#[test]
fn 提交用的遮罩跟着照片一起换坐标系() {
    let (dir, ctx, img) = fixture("submask");
    // 遮罩按 proxy 分辨率存；这张 40×20 没有单独的 proxy 档，就按源图尺寸给
    let mut m = Rgba::new(40, 20);
    for y in 0..20usize {
        for x in 0..40usize {
            let v = if x < 20 { 255u8 } else { 0u8 };
            m.set(x, y, [v, v, v, v]);
        }
    }
    let mrel = util::rel_path(&["projects".into(), "1".into(), "p_mask.png".into()]);
    std::fs::write(dir.join(&mrel), codec::encode_png(&m)).unwrap();
    rimg::set_mask(&ctx, img.id, Some(&mrel)).unwrap();
    let row = rimg::by_id(&ctx, img.id).unwrap().unwrap();
    let file = std::fs::read(dir.join(&mrel)).unwrap();
    // 1) 没有几何段：一个字节都不该动（与 0.2.1 那条上传逐位一致）
    radj::put(&ctx, row.id, &EditOps::default().to_json()).unwrap();
    assert_eq!(submit_mask(&ctx, &row).unwrap(), file, "没转没裁的时候遮罩被重编码了");
    // 2) 右转 90°：照片会换成 20×40，遮罩必须一起换，笔迹从"左半"变成"上半"
    let mut ops = EditOps::default();
    ops.geometry.rotate_deg = 90.0;
    radj::put(&ctx, row.id, &ops.to_json()).unwrap();
    let d = codec::decode(&submit_mask(&ctx, &row).unwrap()).unwrap();
    assert_eq!((d.w, d.h), (20, 40), "遮罩没跟着照片换边长，工作流圈到的就是另一块");
    assert_eq!(d.get(0, 0)[0], 255, "源图左半的笔迹转过去应当落在上半");
    assert_eq!(d.get(0, 30)[0], 0, "下半原本没涂，转过去也不该有笔迹");
    cleanup(ctx, &dir);
}

#[test]
fn 关键点是空的时候一键塑形不改动图() {
    let (dir, ctx, img) = fixture("noshape");
    let mut ops = EditOps::default();
    ops.warp.auto.face_slim = photoedit_core::Slider(80);
    // image_landmark 里一行都没有 → shape_for 给 None → 链上这一段跳过
    let src = codec::decode(&std::fs::read(ctx.data.join(&img.orig_path)).unwrap()).unwrap();
    let out = pixels(&ctx, &img, &ops, Grade::Full).unwrap();
    assert_eq!(out.px, src.px, "没有关键点却动了像素");
    cleanup(ctx, &dir);
}

#[test]
fn separate_face_rows_are_never_merged_for_main_shape() {
    let (dir,ctx,img)=fixture("separatefaces");
    let rows=vec![json!({"points": vec![[0.2,0.3];234]}),json!({"points":vec![[0.7,0.3];234]})];
    radj::put_analysis(&ctx,img.id,&[],&rows).unwrap();
    assert!(crate::service::face::cached_shape(&ctx,&img).is_none());
    let ops=EditOps::parse(r#"{"beauty":{"brighten":100,"auto_face":true}}"#).unwrap().0;
    let original=orig_rgba(&ctx,&img).unwrap();
    assert_eq!(pixels(&ctx,&img,&ops,Grade::Full).unwrap(),original);
    let snapshot=snapshot_inputs(&ctx,&img,&ops).unwrap();
    let hash_before=render_hash(&ctx,&img,&ops).unwrap();
    let (_,record)=input_slot(&ctx,&img,&hash_before);
    imagesvc::write_bytes(&record,b"immutable submitted photo").unwrap();
    // Late analysis must not change the saved skin input.
    radj::put_analysis(&ctx,img.id,&[],&[]).unwrap();
    assert_ne!(hash_before,render_hash(&ctx,&img,&ops).unwrap());
    assert_eq!(std::fs::read(&record).unwrap(),b"immutable submitted photo");
    assert_eq!(photo_from_snapshot(&ctx,&img,&ops,&snapshot,None).unwrap(),original);
    drop(ctx);
    std::fs::remove_dir_all(dir).unwrap();
}

/// 一张明显大于 proxy 档的图：源 1600×1200，proxy 只有 400×300。
/// 0.3.0 那版「本地调整只剩一张小图、放大看不到真像素」就出在这种尺寸差的组合上，
/// 40×20 的小图测不出来——它的 proxy 档与源图同尺寸，两个口径报出来的数一样。
fn big_fixture(name: &str) -> (std::path::PathBuf, std::sync::Arc<Ctx>, Image) {
    let dir = std::env::temp_dir().join(format!("synco-adj-{name}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("projects").join("1")).unwrap();
    let ctx = Ctx::new(dir.clone(), dir.clone(), db::open(&dir).unwrap());
    crate::repo::projects::create(&ctx, "测试项目").unwrap();
    let mut src = Rgba::new(1600, 1200);
    for y in 0..1200usize {
        for x in 0..1600usize {
            src.set(x, y, [(x / 7) as u8, (y / 5) as u8, 90, 255]);
        }
    }
    let rel = util::rel_path(&["projects".into(), "1".into(), "big.png".into()]);
    std::fs::write(dir.join(&rel), codec::encode_png(&src)).unwrap();
    let prel = util::rel_path(&["projects".into(), "1".into(), "big_proxy400.jpg".into()]);
    std::fs::write(dir.join(&prel), codec::encode_jpeg(&codec::scale_to_long_edge(&src, 400), 88)).unwrap();
    let id = rimg::insert(&ctx, 1, "big.png", &rel, 1600, 1200).unwrap();
    rimg::set_derived(&ctx, id, None, Some(&prel)).unwrap();
    let row = rimg::by_id(&ctx, id).unwrap().unwrap();
    (dir, ctx, row)
}

#[test]
fn 预览报的是成图那一档的尺寸而不是proxy档() {
    let (dir, ctx, img) = big_fixture("prevdims");
    assert!(img.proxy_path.is_some(), "fixture 没接上 proxy 档");
    let mut ops = EditOps::default();
    ops.color.exposure = photoedit_core::Slider(40);
    let b = build_preview(&ctx, &img, &ops).unwrap();
    // 关键一条：预览档只有 400×300，报出去的必须是 1600×1200。
    // 报了档自己的尺寸，前端就把它当画幅用——图当场小一圈，1:1 也只是那张糊图
    assert_eq!((b.w, b.h), (1600, 1200));
    let again = build_preview(&ctx, &img, &ops).unwrap();
    assert!(again.reused && (again.w, again.h) == (1600, 1200), "复用那条分支与新建口径不一致");
    // 转过 90° 长宽换边的还是成图那一档，不是 proxy 那一档
    let mut rot = ops.clone();
    rot.geometry.rotate_deg = 90.0;
    let t = build_preview(&ctx, &img, &rot).unwrap();
    assert_eq!((t.w, t.h), (1200, 1600));
    cleanup(ctx, &dir);
}

#[test]
fn 调整视图的瓦片切的是成图那一档() {
    let (dir, ctx, img) = big_fixture("adjtiles");
    let mut ops = EditOps::default();
    ops.beauty.smooth = photoedit_core::Slider(50);
    let m = tiles(&ctx, &img, &ops).unwrap();
    assert_eq!(m["w"].as_i64(), Some(1600));
    assert_eq!(m["h"].as_i64(), Some(1200));
    assert_eq!(m["tile"].as_i64(), Some(imagesvc::TILE as i64));
    let url = m["url"].as_str().unwrap_or_default().to_string();
    assert!(url.contains(&format!("cache/t/{}/adj", img.id)), "瓦片路径没带参数指纹：{url}");
    let levels = m["levels"].as_array().cloned().unwrap_or_default();
    assert!(!levels.is_empty(), "长边 1600 该切出一套金字塔");
    // 末元素是最细一层（build_tiles_at 把顺序反转过）
    let z = levels[levels.len() - 1]["z"].as_i64().unwrap_or(-1);
    let hash = ops_hash(&ops.to_json());
    let root = imagesvc::tiles_dir(&ctx, &img);
    let finest = root.join(format!("adj{hash}")).join(z.to_string()).join("0_0.jpg");
    assert!(finest.is_file(), "最细一层的第一格没落盘：{}", finest.display());
    // 换一套参数：新目录建起来，旧的那套要被请走——一次就是上百张，留着等于白堆
    let mut other = ops.clone();
    other.color.contrast = photoedit_core::Slider(30);
    let m2 = tiles(&ctx, &img, &other).unwrap();
    assert_ne!(m2["url"].as_str().unwrap_or_default(), url.as_str());
    let root = imagesvc::tiles_dir(&ctx, &img);
    assert!(root.join(format!("adj{}", ops_hash(&other.to_json()))).is_dir());
    assert!(!root.join(format!("adj{hash}")).exists(), "旧参数的瓦片目录没清掉");
    cleanup(ctx, &dir);
}
