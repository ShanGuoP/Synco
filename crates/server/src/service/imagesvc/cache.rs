//! 可再生缓存统计、限额清理与旧派生档回收。
use crate::models::entity::Image;
use crate::state::Ctx;
use crate::repo;
use std::path::{Path, PathBuf};
/// 可再生档的家：`data/runtime/cache/`。这一层里的东西**随时可以删**——原图、遮罩、窗口原图、
/// 参数快照与数据库都在别处，删掉的代价只是下一次要看时重切一次（24MP 实测发布档约 0.5 秒）。
/// 挑 `runtime/` 是因为它的定义本来就是"进程私有、备份可以不带"，正好对上缓存的语义。
///
/// 判据是"删掉之后**任何时候**都还能算回来"，不是"眼下算得回来"：
/// thumb/proxy 只有原图还在时才能重切（误删事故后那批行就是靠它留着最后的像素），
/// `_adjinput` 那一张的参数会被用户改掉就再也渲不回——所以这两类都留在 `projects/`，
/// 这一层只收瓦片与预览/成图那种"看一眼、下次再看会重算"的档。
pub fn cache_dir(ctx: &Ctx) -> PathBuf {
    crate::runtime_dir(&ctx.data).join("cache")
}

/// 瓦片目录：按 image_id 归位，id 变了（重新导入）就不会命中旧缓存
pub fn tiles_dir(ctx: &Ctx, img: &Image) -> PathBuf {
    cache_dir(ctx).join("t").join(img.id.to_string())
}

/// 缓存里"一张图"粒度的条目集合：`t/<id>` 与 `s/<id>` 底下那一层目录
fn cache_entries(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for group in ["t", "s"] {
        let Ok(rd) = std::fs::read_dir(root.join(group)) else { continue };
        for e in rd.flatten() {
            if e.path().is_dir() {
                out.push(e.path());
            }
        }
    }
    out
}

fn tree_stat(dir: &Path) -> (u64, std::time::SystemTime) {
    let mut bytes = 0u64;
    let mut newest = std::time::UNIX_EPOCH;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if let Ok(md) = e.metadata() {
                bytes += md.len();
                if let Ok(t) = md.modified() {
                    if t > newest {
                        newest = t;
                    }
                }
            }
        }
    }
    (bytes, newest)
}

/// 缓存层现在占多少（设置页要说清"清掉能省多少"）
pub fn cache_bytes(ctx: &Ctx) -> u64 {
    cache_entries(&cache_dir(ctx)).iter().map(|p| tree_stat(p).0).sum()
}

/// 把缓存压进上限之内，**最久没重切过的那张先丢**。
///
/// 粒度是"一张图"：半套瓦片比一整套更糟（屏幕上会看见没切完的那一格），所以整目录一起走。
/// 时间取目录里最新的文件时间——重切会更新它，只读不更新，所以这是"最近重算过"的近似，
/// 不是"最近看过"的精确；缓存满了要腾地方，近似够用。
pub fn cache_sweep(ctx: &Ctx, cap_bytes: u64) -> (usize, u64) {
    if cap_bytes == 0 {
        return (0, 0);
    }
    let root = cache_dir(ctx);
    let mut rows: Vec<(std::time::SystemTime, u64, PathBuf)> =
        cache_entries(&root).into_iter().map(|p| {
            let (b, t) = tree_stat(&p);
            (t, b, p)
        }).collect();
    let total: u64 = rows.iter().map(|r| r.1).sum();
    if total <= cap_bytes {
        return (0, 0);
    }
    rows.sort_by_key(|r| r.0);
    let mut freed = 0u64;
    let mut gone = 0usize;
    for (_, b, p) in rows {
        if total - freed <= cap_bytes {
            break;
        }
        if std::fs::remove_dir_all(&p).is_ok() {
            freed += b;
            gone += 1;
        }
    }
    (gone, freed)
}

/// 全清：设置页那颗「清掉可再生档」。只碰 cache_dir，`projects/` 一个字节都不动
pub fn cache_clear(ctx: &Ctx) -> (usize, u64) {
    let root = cache_dir(ctx);
    let mut gone = 0usize;
    let mut freed = 0u64;
    for p in cache_entries(&root) {
        let (b, _) = tree_stat(&p);
        if std::fs::remove_dir_all(&p).is_ok() {
            gone += 1;
            freed += b;
        }
    }
    (gone, freed)
}

/// 缓存上限（MB）：`0` = 不限。默认 4 GB——比这小的照片集根本碰不到清理
pub fn cache_cap(ctx: &Ctx) -> u64 {
    repo::settings::get(ctx, "cache_cap_mb")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(4096)
        .saturating_mul(1048576)
}

/// 这一族名字是"看一眼就扔"的档：<父图名>_<档种><8 位十六进制指纹>.jpg
fn view_artifact(name: &str) -> bool {
    let Some(rest) = name.strip_suffix(".jpg") else { return false };
    ["_adjprev", "_adjusted", "_adjthumb"].iter().any(|k| {
        rest.find(k).map(|i| {
            let tail = &rest[i + k.len()..];
            tail.len() == 8 && tail.chars().all(|c| c.is_ascii_hexdigit())
        }).unwrap_or(false)
    })
}

fn stat_bytes(path: &Path) -> u64 {
    if path.is_dir() { tree_stat(path).0 } else { path.metadata().map(|m| m.len()).unwrap_or(0) }
}

/// 派生档换了家之后，旧家留下的孤儿收一次：`projects/<号>/tiles/` 整棵，加上散在项目目录里的
/// 预览/成图/成图小档。这些在新家会现切现渲，删掉不丢任何一个像素。
///
/// 名单里没有 `_adjinput`（结果行的 `orig_path` 指着它，参数一改就再也渲不回那一张），
/// 没有 `_thumb`/`_proxy`（原图一旦没了就只剩它，误删事故那批行就是例子），成图那一族更不碰。
/// 判据也收得很紧：尾巴必须是 8 位十六进制指纹再接 `.jpg`，用户自己起的名字删不掉。
pub fn sweep_legacy_view_files(ctx: &Ctx) -> (usize, u64) {
    let mut gone = 0usize;
    let mut freed = 0u64;
    let Ok(projects) = std::fs::read_dir(ctx.data.join("projects")) else { return (0, 0) };
    for p in projects.flatten() {
        let Ok(entries) = std::fs::read_dir(p.path()) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_lowercase();
            let hit = (path.is_dir() && name == "tiles") || (path.is_file() && view_artifact(&name));
            if !hit {
                continue;
            }
            let bytes = stat_bytes(&path);
            let removed = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) }.is_ok();
            if removed {
                gone += 1;
                freed += bytes;
            }
        }
    }
    (gone, freed)
}
