#![cfg(test)]
//! 模块行为与边界回归。
use super::*;

/// 导出重算出来的那一张，必须与当年落盘那一张**逐位相同**——不然"无损导出"是句空话：
/// 用户拿到一张看着一样、像素不一样的图，而文件名后缀还写着 .png。
/// 顺手钉住另外两条：成图落盘改 q95 后原件靠 raw + 遮罩快照 + 参数快照三样拼回来；
/// 遮罩在提交之后被人改过笔迹，也不得把重算的那一张带跑。
#[test]
fn 重算无损与当时那一张逐位相同() {
    let dir = std::env::temp_dir().join(format!("synco-lossless-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = crate::state::Ctx::new(dir.clone(), dir.clone(), crate::repo::db::open(&dir).unwrap());
    let pid = crate::repo::projects::create(&ctx, "无损").unwrap();
    let proj = dir.join("projects").join(pid.to_string());
    std::fs::create_dir_all(&proj).unwrap();
    let mut orig = stitch_core::Rgba::new(400, 300);
    for y in 0..300usize {
        for x in 0..400usize {
            let o = (y * 400 + x) * 4;
            orig.px[o] = x as u8;
            orig.px[o + 1] = y as u8;
            orig.px[o + 2] = 128;
            orig.px[o + 3] = 255;
        }
    }
    std::fs::write(proj.join("a.png"), codec::encode_png(&orig)).unwrap();
    let mut mask = stitch_core::Rgba::new(400, 300);
    for y in 100..180usize {
        for x in 120..260usize {
            mask.px[(y * 400 + x) * 4 + 3] = 255;
        }
    }
    std::fs::write(proj.join("a_mask.png"), codec::encode_png(&mask)).unwrap();
    let iid = crate::repo::images::insert(&ctx, pid, "a.png", &format!("projects/{pid}/a.png"), 400, 300).unwrap();
    crate::repo::images::set_mask(&ctx, iid, Some(&format!("projects/{pid}/a_mask.png"))).unwrap();
    let img = rimg::by_id(&ctx, iid).unwrap().unwrap();
    let settings = serde_json::json!({ "edge": 512, "invert": false });
    let pre = prepare(&ctx, &img, &settings).unwrap();
    // 模型回来的那张：拿发出去的裁切图反一次色，字节确定
    let mut model = codec::decode(&pre.crop).unwrap();
    for p in model.px.chunks_exact_mut(4) {
        let (a, b, c) = (p[0], p[1], p[2]);
        p[0] = 255 - a;
        p[1] = 255 - b;
        p[2] = 255 - c;
    }
    let model_png = codec::encode_png(&model);
    let want = codec::encode_png(&stitch_crop(&pre.base, &pre.alpha, pre.crop_box, &model, &pre.params));
    let rid = rres::insert_cloud(&ctx, iid, pid, "p", &settings.to_string(), "m", None).unwrap();
    compose(&ctx, rid, &img, &pre, &model_png).unwrap();
    let row = rres::by_id(&ctx, rid).unwrap().unwrap();
    assert!(row.raw_path.as_deref().unwrap_or("").ends_with("_raw.png"), "窗口原图没落盘");
    assert!(row.mask_snap_path.is_some(), "遮罩快照没落盘");
    assert!(row.final_path.as_deref().unwrap_or("").ends_with(".jpg"), "成图那一份该是 q95");
    let got = lossless_bytes(&ctx, &row).unwrap();
    assert!(got.lossless, "重算这一条该报无损：{}", got.why.unwrap_or(""));
    assert_eq!(got.ext, "png");
    assert_eq!(got.bytes, want, "重算的那一张与当时落盘的逐位不同");

    // 提交之后又涂了一笔：重算必须还按当时那份快照走
    let mut later = mask.clone();
    for y in 0..40usize {
        for x in 0..40usize {
            later.px[(y * 400 + x) * 4 + 3] = 255;
        }
    }
    std::fs::write(proj.join("a_mask.png"), codec::encode_png(&later)).unwrap();
    let again = lossless_bytes(&ctx, &row).unwrap();
    assert_eq!(again.bytes, want, "遮罩漂移把重算的那一张带跑了");

    // 原图被手动删过：给不出无损，必须明说为什么，并把 q95 那一份照交
    std::fs::remove_file(proj.join("a.png")).unwrap();
    let deg = lossless_bytes(&ctx, &row).unwrap();
    assert!(!deg.lossless);
    assert_eq!(deg.ext, "jpg");
    assert_eq!(deg.why, Some("srv.lossless.origGone"));
    drop(ctx);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 并发从 3 下调到 1 时若还有 3 张在飞：增量式调整一个许可也削不掉，
/// 等它们跑完 available 会回到 3（设置写串行、实际同发 3 张），再往上调还能冲破上限。
/// 闸门与在飞计数都挂在各自新建的 `Ctx` 上——它们曾是模块级 static，
/// 于是这条测试与任何碰闸门的测试都有顺序依赖，跑的顺序一变结论就变。
#[test]
fn 在飞时下调并发不会漏发也不冲破上限() {
    let dir = std::env::temp_dir().join(format!("synco-gate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = crate::state::Ctx::new(dir.clone(), dir.clone(), crate::repo::db::open(&dir).unwrap());
    sync_gate(&ctx, 3);
    assert_eq!(ctx.gate.available_permits(), 3);
    let held: Vec<_> = (0..3).map(|_| ctx.gate.clone().try_acquire_owned().unwrap()).collect();
    ctx.live.fetch_add(3, Ordering::AcqRel);
    sync_gate(&ctx, 1);
    assert_eq!(ctx.gate.available_permits(), 0, "三张在飞时不该还有空位");
    drop(held);
    ctx.live.fetch_sub(3, Ordering::AcqRel);
    sync_gate(&ctx, 1);
    assert_eq!(ctx.gate.available_permits(), 1, "释放之后只留一个坑");
    sync_gate(&ctx, 6);
    assert_eq!(ctx.gate.available_permits(), 6, "上调要能一路开到上限");
    sync_gate(&ctx, 1);
    assert_eq!(ctx.gate.available_permits(), 1);
    drop(ctx);   // 连接还开着的时候 Windows 删不掉 app.db
    std::fs::remove_dir_all(&dir).ok();
}

/// 建画布时按同一口径拒过，提交时再判一次：库里可能躺着别的入口写进来的行
#[test]
fn 画布尺寸按云端硬约束判() {        assert!(sketch_fit(1024, 1024).is_ok());
    assert!(sketch_fit(2048, 1536).is_ok(), "3:2 的 3MP 该过");
    assert!(sketch_fit(4000, 6000).is_err(), "长边与像素都顶破了");
    assert!(sketch_fit(800, 200).is_err(), "4:1 超比例");
    assert!(sketch_fit(700, 700).is_err(), "49 万像素低于下限");
    assert!(sketch_fit(0, 0).is_err());
    assert!(sketch_fit(1, 4000).is_err());
}

/// 原样发的两条判据：签名不对（相机直出的 JPEG/WEBP）或体积顶穿中转上限，都必须回落重编码
#[test]
fn 原样发只认真正的小png() {
    let png = |n: usize| [&PNG_SIG[..], &vec![7u8; n][..]].concat();
    assert!(raw_sendable(&png(1024)));
    assert!(!raw_sendable(&[0xff, 0xd8, 0xff, 0xe0, 0, 0][..]), "JPEG 不能被声明成 PNG 发出去");
    assert!(!raw_sendable(&[]), "空字节不是 PNG");
    assert!(!raw_sendable(&png(cloud::SEND_MAX_BYTES + 1)), "超上限就不该原样发");
    assert!(raw_sendable(&png(cloud::SEND_MAX_BYTES - PNG_SIG.len())), "正好卡在上限内的该放行");
}

/// 整图重绘的目标尺寸：判据是纯函数，所以能一条不打折地钉住
#[test]
fn 整图重绘的目标尺寸永远落在云端框里() {
    use stitch_core::geom::{EDGE_MAX, PX_MAX, PX_MIN, RATIO_MAX};
    let cases = [
        (1200usize, 800usize, 2048u32),
        (1200, 800, 1024),
        (4000, 1400, 4096),
        (3368, 6000, 1024),
        (3840, 3840, 3840),
        (900, 900, 512),
    ];
    for (w, h, edge) in cases {
        let (fw, fh, same) = super::full_target(w, h, edge).unwrap_or_else(|e| panic!("{w}×{h}@{edge}：{}", e.text()));
        let long = fw.max(fh) as usize;
        let ratio = long as f64 / fw.min(fh) as f64;
        let px = fw as u64 * fh as u64;
        assert!(long <= EDGE_MAX as usize, "{w}×{h}@{edge} → {fw}×{fh} 长边顶破");
        assert!(ratio <= RATIO_MAX + 0.02, "{w}×{h}@{edge} → {fw}×{fh} 比例 {ratio}");
        assert!((PX_MIN..=PX_MAX).contains(&px), "{w}×{h}@{edge} → {fw}×{fh} 像素 {px} 出界");
        assert_eq!(same, (fw as usize, fh as usize) == (w, h), "{w}×{h}@{edge} 的\"原样发\"判据要一致");
    }
}

#[test]
fn 已经在框里的原图不重编码() {
    // 96 万像素、比例 1.5：本来就在框内，edge 给到 2048 时应当一个像素都不动地原样发
    let (w, h, passthrough) = super::full_target(1200, 800, 2048).unwrap();
    assert_eq!((w, h), (1200, 800));
    assert!(passthrough, "已经在框里就该原样发");
    // 同一张图把目标长边压到 1024：要折，且不再算"原样"
    let (w2, h2, same2) = super::full_target(1200, 800, 1024).unwrap();
    assert_eq!(w2, 1024, "长边该按这一行选的档位折");
    assert!(h2 > 0 && !same2);
}

/// 比例不可能合法时给可读理由，而不是硬发一张会被对面拒的图
#[test]
fn 折不进框的整图给可读理由() {
    let e = super::full_target(4000, 400, 2048).unwrap_err().text();
    assert!(e.contains("4000×400"), "{e}");
    assert!(e.contains("折不进"), "{e}");
}
