#![cfg(test)]
//! 模块行为与边界回归。
use super::*;
use stitch_core::Rgba;

#[test]
fn 层级尺寸从粗到细且封顶是原尺寸() {
    let v = level_sizes(4000, 6000);
    assert_eq!(*v.last().unwrap(), (4000, 6000));
    assert!(v[0].0.max(v[0].1) <= TILE);
    // 由粗到细逐级翻倍（向上取整），所以细级的一半就是上一级
    for i in 1..v.len() {
        assert_eq!(v[i].0.div_ceil(2), v[i - 1].0);
        assert_eq!(v[i].1.div_ceil(2), v[i - 1].1);
    }
    assert_eq!(level_sizes(300, 200), vec![(300, 200)]);
    assert_eq!(level_sizes(0, 0), vec![(0, 0)]);
}

#[test]
fn 命名规律决定缓存分级() {
    assert_eq!(cache_policy("projects/1/a_mask.png").1, true);
    assert_eq!(cache_policy("projects/1/1234_ab_photo.jpg").1, false);
    assert!(cache_policy("projects/1/a_mask.png").0.contains("no-cache"));
    assert!(cache_policy("projects/1/tiles/3/4/0_0.jpg").0.contains("immutable"));
    // 画稿会被原地覆写（自动保存、取回快照），一年期 immutable 会把旧笔迹钉在浏览器里
    assert!(cache_policy("projects/1/1700_ab_画稿_sketch.png").0.contains("no-cache"));
    assert_eq!(cache_policy("projects/1/1700_ab_画稿_sketch.png").1, true);
}

/// 一张 4000×6000（24MP，就是你 data 里那个规格）走完整条派生链：
/// thumb/proxy/瓦片都要出文件，层级和瓦片数要数得对。跑一次把耗时打出来，别靠估。
#[test]
fn 二四mp_的派生档与瓦片全链() {
    use std::time::Instant;
    let dir = std::env::temp_dir().join(format!("synco-big-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = crate::state::Ctx::new(dir.clone(), dir.clone(), crate::repo::db::open(&dir).unwrap());
    let pid = crate::repo::projects::create(&ctx, "大图").unwrap();
    let (w, h) = (4000usize, 6000usize);
    // 渐变叠噪点：纯色对 JPEG 太友好，耗时会假得像成功了
    let mut img = Rgba::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let n = ((x * 7 + y * 13) % 251) as u8;
            img.set(x, y, [(x % 255) as u8, (y % 255) as u8, n, 255]);
        }
    }
    let rel = util::rel_path(&["projects".into(), pid.to_string(), "big.png".into()]);
    std::fs::create_dir_all(dir.join("projects").join(pid.to_string())).unwrap();
    std::fs::write(dir.join(&rel), codec::encode_png(&img)).unwrap();
    let iid = crate::repo::images::insert(&ctx, pid, "big.png", &rel, w as i64, h as i64).unwrap();
    let row = crate::repo::images::by_id(&ctx, iid).unwrap().unwrap();

    let t = Instant::now();
    assert!(derive(&ctx, &row).unwrap(), "thumb 应当被生成");
    let derive_ms = t.elapsed().as_millis();
    let after = crate::repo::images::by_id(&ctx, iid).unwrap().unwrap();
    assert!(after.thumb_path.as_deref().map(|p| dir.join(p).is_file()).unwrap_or(false), "thumb 要落盘并入库");
    assert!(after.proxy_path.as_deref().map(|p| dir.join(p).is_file()).unwrap_or(false), "24MP 超出档位，proxy 必须出");

    let t = Instant::now();
    let meta = tiles(&ctx, &after).unwrap();
    let tiles_ms = t.elapsed().as_millis();
    assert_eq!(meta["tile"], serde_json::json!(TILE));
    let levels = meta["levels"].as_array().unwrap();
    assert_eq!(levels.last().unwrap()["w"].as_u64().unwrap() as usize, w);
    let mut want = 0usize;
    for l in levels {
        want += (l["cols"].as_u64().unwrap() as usize) * (l["rows"].as_u64().unwrap() as usize);
    }
    let mut got = 0usize;
    count_jpg(&tiles_dir(&ctx, &after), &mut got);
    assert_eq!(got, want, "清单里数出来的瓦片数要和盘上的一一对应");
    assert!(got > 100, "24MP 至少该切出上百张，实得 {got}");
    println!("  24MP 实测（debug 构建）：thumb+proxy {derive_ms}ms，瓦片 {tiles_ms}ms（{got} 张 / {} 级）", levels.len());
    drop(ctx);   // 58MB 的目录以前就是这么一年年留在临时盘上的：连接没撒手，删除在 Windows 上静默失败
    let _ = std::fs::remove_dir_all(&dir);
}

/// 缓存层的两条规矩：回收**只碰** `runtime/cache/`，丢的是最久没重切的那一张，而且是整目录走
/// （半套瓦片比一整套更糟）。`projects/` 里那些不可再生的东西永远不可能被这条扫到。
#[test]
fn 缓存按上限回收_只碰缓存层() {
    let dir = std::env::temp_dir().join(format!("synco-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = crate::state::Ctx::new(dir.clone(), dir.clone(), crate::repo::db::open(&dir).unwrap());
    // 不可再生的那份放 projects/：清理扫到它就算越界
    std::fs::create_dir_all(dir.join("projects").join("1")).unwrap();
    std::fs::write(dir.join("projects").join("1").join("a.png"), vec![b'x'; 4096]).unwrap();
    // 三张图的瓦片目录，各 1024 字节；后建的"更新"
    let root = cache_dir(&ctx).join("t");
    for id in 1..=3 {
        let d = root.join(id.to_string());
        std::fs::create_dir_all(&d).unwrap();
        for k in 0..2 {
            std::fs::write(d.join(format!("{k}.jpg")), vec![b'x'; 512]).unwrap();
        }
    }
    assert_eq!(cache_bytes(&ctx), 3072, "该数出三张图的量");
    // 上限 1600：先丢最久的那张（1024）还超，再丢第二张，剩一张才停
    let (gone, freed) = cache_sweep(&ctx, 1600);
    assert_eq!((gone, freed), (2, 2048), "该丢掉最久没重切的两处");
    assert!(root.join("3").is_dir(), "最新的那张被误丢了");
    assert!(!root.join("1").exists() && !root.join("2").exists(), "旧目录没走干净");
    assert!(dir.join("projects").join("1").join("a.png").is_file(), "清理伸进 projects/ 了");
    // 0 = 不限：多大的缓存都不该动
    assert_eq!(cache_sweep(&ctx, 0), (0, 0));
    let (n, b) = cache_clear(&ctx);
    assert_eq!((n, b), (1, 1024), "手动清一次该把剩下那张带走");
    assert_eq!(cache_bytes(&ctx), 0);
    drop(ctx);   // 连接没撒手的话 Windows 删不掉 app.db
    let _ = std::fs::remove_dir_all(&dir);
}

/// 搬家之后旧家的孤儿只收"看一眼"那一族。样张跨在判据两侧：尾巴不是 8 位十六进制的
/// 用户文件名、被结果行指着的 `_adjinput`、原图没了就只剩它的 `_thumb`/`_proxy`，都必须在。
#[test]
fn 旧位置的派生档只收那一族() {
    let dir = std::env::temp_dir().join(format!("synco-legacy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = crate::state::Ctx::new(dir.clone(), dir.clone(), crate::repo::db::open(&dir).unwrap());
    let p1 = dir.join("projects").join("1");
    std::fs::create_dir_all(p1.join("tiles").join("3").join("0")).unwrap();
    std::fs::write(p1.join("tiles").join("3").join("0").join("0_0.jpg"), vec![b'x'; 700]).unwrap();
    let keep = ["a.png", "a_mask.png", "a_thumb.jpg", "a_proxy1024.jpg",
        "a_adjinputdeadbeef.jpg",              // 结果行的 orig_path 指着它
        "旅行 照片_adjprevzzzzzzzz.jpg"];      // 尾巴不是十六进制指纹：判据两侧的另一半，用户的东西
    for n in &keep { std::fs::write(p1.join(n), vec![b'x'; 100]).unwrap(); }
    let go = ["a_adjprev1a2b3c4d.jpg", "a_adjustedDEADBEEF.jpg", "a_adjthumb00112233.jpg"];
    for n in &go { std::fs::write(p1.join(n), vec![b'x'; 300]).unwrap(); }
    // 这条是全局走的，判据只认名字那一族：别的项目里被库指着的一样不许扫掉
    std::fs::create_dir_all(dir.join("projects").join("2")).unwrap();
    std::fs::write(dir.join("projects").join("2").join("c.png"), vec![b'x'; 300]).unwrap();
    std::fs::write(dir.join("projects").join("2").join("c_adjinput0000abcd.jpg"), vec![b'x'; 300]).unwrap();

    let (n, b) = sweep_legacy_view_files(&ctx);
    // 4 处：三个渲染档 + 那一棵 tiles/（用户文件与记录都不进名单，删了就是事故）
    assert_eq!(n, 3 + 1, "该请走的：预览/成图/成图小档 + tiles/");
    assert_eq!(b, 300 * 3 + 700, "字节账要连瓦片子目录一起数：{b}");
    for name in &keep { assert!(p1.join(name).is_file(), "{name} 不该被碰"); }
    for name in &go { assert!(!p1.join(name).exists(), "{name} 还留在旧家"); }
    assert!(!p1.join("tiles").exists(), "旧瓦片目录没收到");
    assert!(dir.join("projects").join("2").join("c.png").is_file(), "别的项目的原图被扫了");
    assert!(dir.join("projects").join("2").join("c_adjinput0000abcd.jpg").is_file(), "提交记录被扫了");
    // 再跑一次什么都找不到：这条是每次启动都走的，不该留下"第二次要少删点"的状态
    assert_eq!(sweep_legacy_view_files(&ctx), (0, 0));
    drop(ctx);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 递归数瓦片目录里的 jpg
fn count_jpg(dir: &std::path::Path, n: &mut usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if e.path().is_dir() {
            count_jpg(&e.path(), n);
        } else if e.path().extension().map(|x| x == "jpg").unwrap_or(false) {
            *n += 1;
        }
    }
}
