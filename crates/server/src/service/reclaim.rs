//! 结果行的推进与回收。跟 HTTP 无关的逻辑不放 api 层：启动收尸也要用同一套。
//! 规则：连判三轮才认连不上、三件套不成套就清孤儿、
//! 没有 prompt_id 的 running 一律判"缝合前断掉"。

use crate::error::Result;
use crate::models::dto;
use crate::repo::results as rres;
use crate::service::{backend, cloud, comfy};
use crate::state::Shared;
use crate::util;
use serde_json::Value;
use std::collections::HashMap;

pub const OUTPUT_TAGS: [&str; 3] = ["final", "crop", "maskoverlay"];

/// 云端行的判定。本进程没有这份在飞的记录（重启前留下的、或 worker 崩在半路），
/// 或者起表超过宽限期还没落盘，就判掉；还在飞的原样返回，让前端继续轮询。
/// 返回 true 表示这张已经被标成 error。
pub(crate) fn judge_cloud(ctx: &Shared, id: i64) -> bool {
    if ctx.job_live(id) && !ctx.job_overdue(id, cloud::grace_ms(ctx)) {
        return false;
    }
    ctx.mark_error(id, "这一张云端没走完（请求断了或超时），重新提交一张");
    true
}

/// 问一次 ComfyUI，把库里这条结果推进到终态
pub async fn settle(ctx: &Shared, id: i64) -> Result<Option<Value>> {
    let Some(r) = rres::by_id(ctx, id)? else { return Ok(None) };
    let pid = r.project_id;
    if r.running() {
        match r.prompt_id {
            // 云端行没有 prompt_id：推进它的只有队列 worker，这里只看它在不在飞
            None => {
                judge_cloud(ctx, id);
            }
            Some(prompt_id) => {
                let c = comfy::check_comfy(ctx, &prompt_id).await;
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
                            ctx.mark_error(id, &format!("连不上 ComfyUI（{}）：{}", backend::active_url(ctx), detail));
                        }
                    }
                    comfy::Check::Queued => {} // 队列里还排着：这轮什么都不做
                    comfy::Check::Running => {}
                    comfy::Check::Lost => ctx.mark_error(
                        id,
                        "ComfyUI 已经不认这条任务（多半随它重启，或被点了\"清空历史\"丢了），重新提交一张",
                    ),
                    comfy::Check::Error(detail) => ctx.mark_error(id, &detail),
                    comfy::Check::Done(images) => {
                        download_three(ctx, id, pid, &images).await?;
                    }
                }
            }
        }
    }
    Ok(rres::value_by_id(ctx, id)?.map(|v| dto::result_json(ctx, &v)))
}

/// 三件套一起下载：断在中途时已经落盘的那张不成套，留着就是没人认领的孤儿文件
async fn download_three(ctx: &Shared, id: i64, pid: i64, images: &[comfy::Tagged]) -> Result<()> {
    let by_tag: HashMap<String, String> = images.iter().map(|i| (i.tag.to_string(), i.url.clone())).collect();
    let missing: Vec<&str> = OUTPUT_TAGS.iter().copied().filter(|t| !by_tag.contains_key(*t)).collect();
    if !missing.is_empty() {
        ctx.mark_error(id, &format!("ComfyUI 少了输出：{}", missing.join("、")));
        return Ok(());
    }
    std::fs::create_dir_all(ctx.data.join("projects").join(pid.to_string())).ok();
    let stamp = format!("r{id}_{}", util::now_ms());
    let files: Vec<(&str, String)> = OUTPUT_TAGS
        .iter()
        .copied()
        .zip(["final", "crop", "mask"].iter().map(|s| util::rel_path(&["projects".into(), pid.to_string(), format!("{stamp}_{s}.png")])))
        .collect();
    let mut fail = String::new();
    for (tag, rel) in &files {
        if let Err(e) = comfy::download_to(ctx, &by_tag[*tag], &ctx.data.join(rel)).await {
            fail = e;
            break;
        }
    }
    if !fail.is_empty() {
        for (_, rel) in &files {
            let _ = std::fs::remove_file(ctx.data.join(rel));
        }
        ctx.mark_error(id, &format!("回传成图失败：{fail}"));
        return Ok(());
    }
    ctx.misses.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    rres::set_files_done(ctx, id, &files[0].1, &files[1].1, &files[2].1)?;
    // 历史列要的是 320 的小档，不是那张 20–33MB 的成图；编码放后台，轮询不该等它
    {
        let ctx = ctx.clone();
        let rel = files[0].1.clone();
        tokio::spawn(async move {
            let _ = tokio::task::spawn_blocking(move || crate::service::imagesvc::result_thumb(&ctx, id, pid, &rel)).await;
        });
    }
    Ok(())
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
            ctx.mark_error(*id, &format!("工坊重启时连不上 ComfyUI（{}）", backend::active_url(ctx)));
        }
        return Ok(n + local.len());
    }
    let live = comfy::live_prompts(ctx).await.ok();
    for (id, pid) in &local {
        if let Some(live) = &live {
            if !live.contains(pid) {
                let c = comfy::check_comfy(ctx, pid).await;
                // 队列和 history 都查不到 = 这条任务随 ComfyUI 重启或被"清空历史"没了；连不上时不轻下结论
                if matches!(c, comfy::Check::Lost) {
                    ctx.mark_error(*id, "ComfyUI 已经不认这条任务（多半随它重启，或被点了\"清空历史\"丢了）");
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
