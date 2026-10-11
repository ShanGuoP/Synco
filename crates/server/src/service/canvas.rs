//! 画布生成的提交用例：冻结输入成功后才发布 queued。
use crate::{error::{AppError, Result}, models::entity::Image, repo::{projects, results}, state::Shared, util};
use super::{cloud, queue, refs};
use serde_json::{json, Value};

struct Preparing { ctx: Shared, id: i64 }
impl Drop for Preparing {
    fn drop(&mut self) { self.ctx.job_end(self.id); }
}

pub async fn generate(ctx: &Shared, img: &Image, prompt: &str, rerun: Option<i64>) -> Result<i64> {
    if !util::file_alive(&ctx.data, &Value::String(img.orig_path.clone())) { return Err(AppError::bad("srv.canvas.sketchGone")); }
    queue::sketch_fit(img.w.max(1) as usize, img.h.max(1) as usize)?;
    let config = cloud::settings(ctx);
    if config.base.is_empty() || config.model.is_empty() || config.key.is_empty() { return Err(AppError::bad("srv.canvas.needCloud")); }
    let slots = refs::slots(ctx, img);
    if let Some(missing) = slots.iter().find(|p| !util::file_alive(&ctx.data, &Value::String(p.to_string()))) {
        return Err(AppError::bad_args("srv.canvas.refsGone", json!({ "missing": missing })));
    }
    let payload = json!({ "prompt": prompt, "negative": "", "steps": 0, "cfg": 0, "loras": [], "canvas": true });
    let id = results::insert_cloud(ctx, img.id, img.project_id, prompt, &payload.to_string(), &config.model, rerun)?;
    // 准备期间读接口也可能推进这条 running 行，它必须能认出此进程仍在负责它。
    ctx.job_begin(id);
    let _preparing = Preparing { ctx: ctx.clone(), id };
    let ctx2 = ctx.clone();
    let img2 = img.clone();
    let outcome = util::blocking(move || -> Result<()> {
        let rel = util::rel_path(&["projects".into(), img2.project_id.to_string(), format!("k{}_{id}_sketch.png", img2.id)]);
        let src = util::data_file(&ctx2.data, &img2.orig_path).ok_or_else(|| AppError::bad("srv.canvas.sketchGoneRaw"))?;
        let mut pending = queue::PendingFiles::new(ctx2.data.clone());
        std::fs::copy(src, pending.track(&rel)).map_err(|e| AppError::fail_detail("srv.common.copyFail", e))?;
        results::set_sketch(&ctx2, id, &rel)?;
        pending.commit();
        if !slots.is_empty() {
            let snapped = refs::snapshot(&ctx2, &img2, id, &slots)?;
            if let Err(e) = results::set_refs(&ctx2, id, &snapped) {
                refs::drop(&ctx2, img2.project_id, &snapped);
                return Err(e);
            }
        }
        queue::queue_canvas(&ctx2, &img2, id, config)?;
        Ok(())
    }).await;
    if let Err(e) = outcome { ctx.mark_error_of(id, &e); return Err(e); }
    projects::touch(ctx, img.project_id)?;
    Ok(id)
}
