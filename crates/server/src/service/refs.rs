//! 画布的参考图槽位：上传也好、从库里挑也好，都在这一刻归一化成"要发出去的那张"并落盘。
//!
//! 三层各管一段：这里只管**文件与集合**，不碰 HTTP、不碰出站。
//! 提交时由队列把当次槽位复制成**这一行自己的**快照（与画稿快照同形），
//! 所以"取回这一版""用这组参数再跑一次"永远拿得到当时那几张，而不是此刻的槽位。

use crate::error::Result;
use crate::img::codec;
use crate::models::entity::{Image, ResultRow};
use crate::repo::{images as rimg, settings as rset};
use crate::service::{cloud, imagesvc};
use crate::state::Ctx;
use crate::util;
use serde_json::Value;
use stitch_core::Rgba;

/// 一次最多带几张参考图。模型侧的张数上限**没实测**（`tools/sketch-probe.js --refs` 就是去测那一族），
/// 先收在够用的档上：多一张就是多一份出站字节与多一段耗时，超了给可读拒绝而不是静默截断。
pub const MAX: usize = 4;

/// 槽位集合存在 app_settings 的键（按画布一行一个）。撤画布时要连着键一起清。
fn key(iid: i64) -> String {
    format!("canvas_refs:{iid}")
}

/// 槽位 rel 必须属于这个画布所在项目：库里/设置里躺着别处的路径时不跟着它读删
fn ours(pid: i64, rel: &str) -> bool {
    util::rel_ok(rel) && rel.starts_with(&format!("projects/{pid}/"))
}

fn slots_of(ctx: &Ctx, pid: i64, iid: i64) -> Vec<String> {
    util::safe_arr(rset::get(ctx, &key(iid)).as_deref())
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .filter(|r| ours(pid, r))
        .collect()
}

/// 这张画布当前的槽位集合（顺序就是发出去的顺序，越界的 rel 一律丢掉）
pub fn slots(ctx: &Ctx, img: &Image) -> Vec<String> {
    slots_of(ctx, img.project_id, img.id)
}

fn save_slots(ctx: &Ctx, iid: i64, rels: &[String]) -> Result<()> {
    let arr: Vec<Value> = rels.iter().map(|r| Value::String(r.clone())).collect();
    rset::put(ctx, &key(iid), &Value::Array(arr).to_string())
}

/// 整组替换（撤单张、清空都走这一条）。集合之外的槽位文件由调用方 `drop` 掉，别留孤儿。
pub fn set(ctx: &Ctx, img: &Image, rels: &[String]) -> Result<()> {
    save_slots(ctx, img.id, rels)
}

/// 把进来的字节变成"要发出去的那一张"：解码 → 折进云端那圈硬约束（长边/比例/像素/16 倍数）→ 重编码 PNG。
/// 小图在这里被**折大**到像素下限、大图被折小，所以参考图不会因为"只是一张 512 的色板"被打回；
/// 只有根本折不出合法档的（全景 >3:1 那种）才拒，理由是发出去也必被中转判死。
/// 拍白底是因为参考图常带 alpha，而 `edits` 收的是不透明图——与画稿发出去前那一步同一条处理。
fn normalize(ctx: &Ctx, bytes: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let dec = codec::decode(bytes)?;
    fold(&dec, cloud::settings(ctx).stitch_edge.max(1) as u32).map(|o| codec::encode_png(&o))
}

/// 折到合法档位：已经在框里就只拍平，不动像素；超了才按档位缩放。
fn fold(dec: &Rgba, edge: u32) -> std::result::Result<Rgba, String> {
    let fit = stitch_core::geom::fit_size(dec.w as u32, dec.h as u32, edge);
    if fit.unfit {
        return Err(format!(
            "参考图 {}×{} 折不进云端允许的框（长边≤{}、比例≤3:1、像素 655360~8294400）",
            dec.w, dec.h, stitch_core::geom::EDGE_MAX
        ));
    }
    let flat = codec::flatten(dec, [255, 255, 255]);
    if fit.w as usize == dec.w && fit.h as usize == dec.h {
        return Ok(flat);
    }
    Ok(stitch_core::resize_rgba(&flat, fit.w as usize, fit.h as usize))
}

fn slot_name(img: &Image) -> String {
    util::rel_path(&[
        "projects".into(),
        img.project_id.to_string(),
        format!("{}_{r4}_ref.png", util::now_ms(), r4 = util::r4()),
    ])
}

/// 落一张新槽位，返回它的 rel。体积在这一步已经有界（重编码 PNG + 折档）。
pub fn add_bytes(ctx: &Ctx, img: &Image, bytes: &[u8]) -> std::result::Result<String, String> {
    let png = normalize(ctx, bytes)?;
    let rel = slot_name(img);
    imagesvc::write_bytes(&ctx.data.join(&rel), &png).map_err(|e| format!("参考图落盘失败：{e}"))?;
    Ok(rel)
}

/// 从库里另一张图取参考：**复制**一份成这个画布的槽位，不是引用那个 id。
/// 原图后来被删、被重新导入，都不该让"这一版参考了哪张"变成说不清的事。
pub fn add_from_image(ctx: &Ctx, img: &Image, src_id: i64) -> std::result::Result<String, String> {
    let src = rimg::by_id(ctx, src_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "那张图已经不在库里了".to_string())?;
    let p = util::data_file(&ctx.data, &src.orig_path)
        .ok_or_else(|| format!("那张图的文件不在磁盘上了：{}", src.name))?;
    let bytes = std::fs::read(&p).map_err(|e| format!("读那张图失败：{e}"))?;
    add_bytes(ctx, img, &bytes)
}

/// 历史条目"回填这组参考图"：把那一行的快照复制成新的槽位文件。
/// 复制而不直接指向快照——快照跟着那条记录删，槽位要是挂在它上面，删记录就会把待提交的一组参考图一起带走。
pub fn add_from_result(ctx: &Ctx, img: &Image, row: &ResultRow) -> std::result::Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for rel in row_refs(row) {
        if !ours(img.project_id, &rel) {
            continue;
        }
        let src = util::data_file(&ctx.data, &rel).ok_or_else(|| "这一版的参考图文件已经不在磁盘上了".to_string())?;
        let bytes = std::fs::read(&src).map_err(|e| format!("读参考图快照失败：{e}"))?;
        out.push(add_bytes(ctx, img, &bytes)?);
    }
    Ok(out)
}

/// 提交这一刻：把当次槽位复制成**这一行自己的**快照，名字带 rid 所以永不覆写。
pub fn snapshot(ctx: &Ctx, img: &Image, rid: i64, slots: &[String]) -> std::result::Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for (i, rel) in slots.iter().enumerate() {
        let src = util::data_file(&ctx.data, rel)
            .ok_or_else(|| format!("参考图文件不在磁盘上了：{rel}"))?;
        let dst = util::rel_path(&[
            "projects".into(),
            img.project_id.to_string(),
            format!("g{}_{rid}_ref{}.png", img.id, i + 1),
        ]);
        std::fs::copy(&src, ctx.data.join(&dst)).map_err(|e| format!("复制参考图失败：{e}"))?;
        out.push(dst);
    }
    Ok(out)
}

/// 把这一版真正发出去的几张读出来（队列与界面上的"这一版带 2 张参考"都读它）
pub fn row_refs(row: &ResultRow) -> Vec<String> {
    let s = row.settings();
    s.get("refs")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// 发出去时的字节：快照已在 normalize 那一步折成合法 PNG，这里只读不算。
pub fn row_payloads(ctx: &Ctx, row: &ResultRow) -> std::result::Result<Vec<Vec<u8>>, String> {
    let mut out = Vec::new();
    for rel in row_refs(row) {
        match util::data_file(&ctx.data, &rel) {
            Some(p) => out.push(std::fs::read(&p).map_err(|e| format!("读参考图失败：{e}"))?),
            // 快照被手动删掉时这一版就少一张：宁可少发一张并留痕，也不要整单因为一张读不到而失败
            None => eprintln!("  这一版的参考图少了文件（#{rid}）：{rel}", rid = row.id),
        }
    }
    Ok(out)
}

/// 给界面的形状：路径 + 能不能取到。文件丢了要说"已丢失"，别摆碎图。
pub fn json_of(ctx: &Ctx, rels: &[String]) -> Vec<Value> {
    rels
        .iter()
        .map(|rel| {
            let mut o = serde_json::Map::new();
            o.insert("path".into(), Value::String(rel.clone()));
            o.insert("url".into(), Value::String(format!("/file/{rel}")));
            o.insert("name".into(), Value::String(util::stem_of(rel)));
            o.insert("dead".into(), Value::Bool(!util::file_alive(&ctx.data, &Value::String(rel.clone()))));
            Value::Object(o)
        })
        .collect()
}

/// 撤下：连着盘上的槽位文件一起删。返回没删掉的（已经不在了）数量，界面不用为这个报错。
pub fn drop(ctx: &Ctx, pid: i64, rels: &[String]) -> usize {
    let mut gone = 0;
    for rel in rels {
        if !ours(pid, rel) {
            continue;
        }
        match util::data_file(&ctx.data, rel) {
            Some(p) => {
                if std::fs::remove_file(p).is_err() {
                    gone += 1;
                }
            }
            None => gone += 1,
        }
    }
    gone
}

/// 撤画布：槽位集合与文件一起清（项目删除那条路径另有 remove_dir_all 兜底）
pub fn clear(ctx: &Ctx, pid: i64, iid: i64) -> Result<()> {
    drop(ctx, pid, &slots_of(ctx, pid, iid));
    rset::put(ctx, &key(iid), "[]")
}

