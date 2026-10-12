//! 工作流稳定入口；解析、文件适配、角色校验与运行时决策分别归子模块。
use crate::error::AppError;
use serde_json::{Map, Value};
mod parse;
mod roles;
mod files;
mod cache;
mod planning;
pub use parse::{api_nodes, defaults, sources, CROP_KEYS, DEFAULTS_RAW, LORA_MAX};
pub use roles::{auto_roles, feeds, has_cycle, merge_roles, role, validate, Role, INJECT_KEYS, KNOWN_CLASSES, ROLES};
pub use files::{inspect, load_api_graph, load_map};
pub use cache::{get_cfg, invalidate, set_workflow_path, workflow_path, Cache};
pub use planning::{plan, plan_at, roles_key, roles_view, saved_roles, set_saved_roles, Plan};
// 界面之外没人拼句子：一条理由 = `{code, args}`，前端逐条按当前语言查字典。
// 清单项、角色名都是这个形状，所以「加载图找不到节点」里的角色名也跟着语言走。
fn why(code: &str, args: Value) -> Value {
    let mut o = Map::new();
    o.insert("code".into(), Value::String(code.to_string()));
    if let Value::Object(m) = args {
        if !m.is_empty() {
            o.insert("args".into(), Value::Object(m));
        }
    }
    Value::Object(o)
}

/// 已经是一条带钥匙的错误了就直接用它自己的钥匙，别再包一句
fn why_err(e: &AppError) -> Value {
    let (code, args) = e.reason();
    let mut o = Map::new();
    o.insert("code".into(), Value::String(code));
    if let Some(a) = args {
        o.insert("args".into(), Value::Object(a));
    }
    Value::Object(o)
}

/// 角色名：`label` 存的现在就是钥匙（UNet/CLIP/VAE 这些没进字典，查不到就原样显示）
fn role_ref(key: &str) -> Value {
    why(role(key).map(|r| r.label).unwrap_or(key), Value::Null)
}

#[cfg(test)]
mod tests;
