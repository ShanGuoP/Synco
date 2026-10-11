//! 结果行的推进与回收。跟 HTTP 无关的逻辑不放 api 层：启动收尸也要用同一套。
//! 规则：连判三轮才认连不上、映射了的输出一张不缺才算成、
//! 没有 prompt_id 的 running 一律判"缝合前断掉"。

use crate::error::{AppError, Result};
use crate::models::dto;
use crate::repo::results as rres;
use crate::service::{backend, comfy};
use crate::state::Shared;
use crate::util;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// 云端行的判定。本进程没有这份在飞的记录（重启前留下的、或 worker 崩在半路），
/// 或者起表超过宽限期还没落盘，就判掉；还在飞的原样返回，让前端继续轮询。
/// 返回 true 表示这张已经被标成 error。
pub(crate) fn judge_cloud(ctx: &Shared, id: i64) -> bool {
    if ctx.job_live(id) && !ctx.job_overdue(id, crate::service::queue::job_grace_ms(ctx, id)) {
        return false;
    }
    ctx.mark_error(id, "srv.reclaim.cloudStalled", serde_json::Value::Null);
    true
}

/// 问一次 ComfyUI，把库里这条结果推进到终态
pub async fn settle(ctx: &Shared, id: i64) -> Result<Option<Value>> {
    let Some(r) = rres::by_id(ctx, id)? else { return Ok(None) };
    let pid = r.project_id;
    let outs = comfy::Outputs::from_settings(&r.settings());
    if r.running() {
        match r.prompt_id {
            // 云端行没有 prompt_id：推进它的只有队列 worker，这里只看它在不在飞
            None => {
                judge_cloud(ctx, id);
            }
            Some(prompt_id) => {
                let c = comfy::check_comfy(ctx, &prompt_id, &outs).await;
                // 只要答上了，之前的"连不上"计数就该清零（守卫必须在 await 之前释放干净）
                if !matches!(c, comfy::Check::Unreachable(_)) {
                    ctx.misses.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                }
                match c {
                    comfy::Check::Unreachable(detail) => {
                        /* ComfyUI 装模型时会短暂无应答，连判三轮不上才认失败，免得把还在跑的任务误报 */
                        let over_limit = {
                            let mut misses = ctx.misses.lock().unwrap_or_else(|e| e.into_inner());
                            let n = misses.get(&id).copied().unwrap_or(0) + 1;
                            if n < 3 {
                                misses.insert(id, n);
                                false
                            } else {
                                misses.remove(&id);
                                true
                            }
                        };
                        if over_limit {
                            ctx.mark_error_of(id, &detail);
                        }
                    }
                    comfy::Check::Queued => {} // 队列里还排着：这轮什么都不做
                    comfy::Check::Running => {}
                    comfy::Check::Lost => ctx.mark_error(
                        id,
                        "srv.reclaim.lostTaskRetry",
                        serde_json::Value::Null,
                    ),
                    comfy::Check::Error(detail) => ctx.mark_error(id, &detail, serde_json::Value::Null),
                    comfy::Check::Done(images) => {
                        download_mapped(ctx, id, pid, &images, &outs).await?;
                    }
                }
            }
        }
    }
    Ok(rres::value_by_id(ctx, id)?.map(|v| dto::result_json(ctx, &v)))
}

/// 按这一行映射了的输出下载：成图一定要有，裁切区/遮罩图只看工作流里有没有那两个节点。
/// 断在中途时已经落盘的那张不成套，留着就是没人认领的孤儿文件。
async fn download_mapped(ctx: &Shared, id: i64, pid: i64, images: &[comfy::Tagged], outs: &comfy::Outputs) -> Result<()> {
    /* 占这一行的回传位：轮询 2.5 秒一轮，而成图 20–33MB 要下好几秒，
       同一行因此会被两轮同时判成 Done 并各自下载一套——先落的那套没人认领，盘上只涨不落 */
    let Some(_dl) = ctx.dl_enter(id) else { return Ok(()) };
    let by_tag: HashMap<String, String> = images.iter().map(|i| (i.tag.to_string(), i.url.clone())).collect();
    let need = outs.required_tags();
    let missing: Vec<&str> = need.iter().copied().filter(|t| !by_tag.contains_key(*t)).collect();
    if !missing.is_empty() {
        ctx.mark_error(id, "srv.reclaim.missingOut", serde_json::json!({ "tags": missing }));
        return Ok(());
    }
    std::fs::create_dir_all(ctx.data.join("projects").join(pid.to_string())).ok();
    let stamp = format!("r{id}_{}", util::now_ms());
    let rel_of = |tag: &str| {
        util::rel_path(&[
            "projects".into(),
            pid.to_string(),
            format!("{stamp}_{}.png", if tag == "maskoverlay" { "mask" } else { tag }),
        ])
    };
    let mut files: Vec<(&str, String)> = Vec::new();
    let mut fail: Option<AppError> = None;
    for tag in &need {
        let rel = rel_of(tag);
        if let Err(e) = comfy::download_to(ctx, &by_tag[*tag], &ctx.data.join(&rel)).await {
            fail = Some(e);
            break;
        }
        files.push((tag, rel));
    }
    if let Some(e) = fail {
        for (_, rel) in &files {
            let _ = std::fs::remove_file(ctx.data.join(rel));
        }
        // 回传那层已经有自己的钥匙（连不上 / 超时 / 写盘失败），原样落库，不再包一句中文
        ctx.mark_error_of(id, &e);
        return Ok(());
    }
    ctx.misses.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    let get = |tag: &str| files.iter().find(|(t, _)| t == &tag).map(|(_, r)| r.as_str());
    // 成图必在（need 一定带着它）；空路径进库就变成"卡片在、点开是空的"
    let Some(final_rel) = get("final") else {
        ctx.mark_error(id, "srv.reclaim.noFinal", serde_json::Value::Null);
        return Ok(());
    };
    if !rres::set_files_done(ctx, id, final_rel, get("crop"), get("maskoverlay"))? {
        /* 这几秒里那一行被点了中断或删掉了：状态已经不属于"在飞"，刚落盘的这套文件没人认领 */
        for (_, rel) in &files {
            let _ = std::fs::remove_file(ctx.data.join(rel));
        }
        return Ok(());
    }
    // 历史列要的是 320 的小档，不是那张 20–33MB 的成图；编码放后台，轮询不该等它
    {
        let ctx = ctx.clone();
        let rel = final_rel.to_string();
        tokio::spawn(async move {
            let _ = tokio::task::spawn_blocking(move || crate::service::imagesvc::result_thumb(&ctx, id, pid, &rel)).await;
        });
    }
    Ok(())
}

/// 推进器隔多久看一眼在飞的行
const ADVANCE_EVERY_MS: u64 = 2_000;
/// 没有行在飞时歇久一点：那条 `SELECT status='running'` 很便宜，但也不必填着 2 秒一轮
const ADVANCE_IDLE_MS: u64 = 5_000;
/// 本机一行的总时限。ComfyUI 一直答"还在跑"却永不落地（队列卡死、显存爆了不返回）时，
/// 到点判死并把原因给用户——`Check::Queued/Running` 那两条分支没有尽头，挂着就是永远挂着。
/// 60 分钟是"2K 60 步外加整套工作流"也够的量，判早了会把真在跑的那张掐掉。
const LOCAL_LIMIT_MS: u128 = 60 * 60 * 1000;

/// 服务端自己推进在飞的行。
/// 以前状态只靠浏览器轮询 `GET /api/results/{id}` 来推：页面一关，已经出图的那张就永远停在
/// "生成中"，卡死的 ComfyUI 也永远不会被判掉；而那条 GET 有副作用，本身就是跨站网页的抓手。
pub fn spawn_advancer(ctx: &Shared) {
    let ctx = ctx.clone();
    tokio::spawn(async move {
        // 这一行第一次被推进器看见的时刻：只用它算总时限，重启后重新起算
        let mut seen: HashMap<i64, u128> = HashMap::new();
        loop {
            let rows = match rres::list_running(&ctx) {
                Ok(v) => v,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(ADVANCE_IDLE_MS)).await;
                    continue;
                }
            };
            let idle = rows.is_empty();
            seen.retain(|id, _| rows.iter().any(|(r, _)| r == id));
            for (id, prompt_id) in &rows {
                match prompt_id {
                    // 云端行：本进程没有在飞的记录、或已过宽限期，就判掉；还在飞的原样不动
                    None => {
                        judge_cloud(&ctx, *id);
                    }
                    Some(_) => {
                        let t0 = *seen.entry(*id).or_insert_with(util::now_ms);
                        if util::now_ms().saturating_sub(t0) > LOCAL_LIMIT_MS {
                            ctx.mark_error(*id, "srv.reclaim.localStuck", serde_json::Value::Null);
                            continue;
                        }
                        let _ = settle(&ctx, *id).await;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(if idle { ADVANCE_IDLE_MS } else { ADVANCE_EVERY_MS })).await;
        }
    });
}

/// 服务重启后内存里的轮询队列就没了，库里挂着的 running 行得有人收：
/// 云端行直接判失败，本机行照常推进
pub async fn reclaim_running(ctx: &Shared) -> Result<usize> {
    let rows = rres::list_running(ctx)?;
    if rows.is_empty() {
        return Ok(0);
    }
    let mut n = 0usize;
    let mut local: Vec<(i64, String)> = Vec::new();
    for (id, prompt_id) in &rows {
        match prompt_id {
            // 没有 prompt_id 的一定是云端行：worker 不在了就没人再推它
            None => {
                judge_cloud(ctx, *id);
                n += 1;
            }
            Some(pid) => local.push((*id, pid.clone())),
        }
    }
    if local.is_empty() {
        return Ok(n);
    }
    if !backend::probe(ctx, &backend::active_url(ctx)).await.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        for (id, _) in &local {
            ctx.mark_error(*id, "srv.reclaim.restartNoComfy", serde_json::json!({ "url": backend::active_url(ctx) }));
        }
        return Ok(n + local.len());
    }
    let live = comfy::live_prompts(ctx).await.ok();
    for (id, pid) in &local {
        if let Some(live) = &live {
            if !live.contains(pid) {
                // 只是判"这条还在不在"，输出节点号取谁都行；行没了就按内置那三个
                let outs = rres::by_id(ctx, *id)?.map(|r| comfy::Outputs::from_settings(&r.settings())).unwrap_or_default();
                let c = comfy::check_comfy(ctx, pid, &outs).await;
                // 队列和 history 都查不到 = 这条任务随 ComfyUI 重启或被"清空历史"没了；连不上时不轻下结论
                if matches!(c, comfy::Check::Lost) {
                    ctx.mark_error(*id, "srv.reclaim.lostTask", serde_json::Value::Null);
                    n += 1;
                    continue;
                }
            }
        }
        let _ = settle(ctx, *id).await;
    }
    Ok(n)
}

// ---------------------------------------------------------------- 目录体检
