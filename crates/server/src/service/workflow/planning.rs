//! 角色设置持久化、提交决策与面板状态装配。
use crate::state::Ctx;
use serde_json::{json, Map, Value};
use std::path::Path;
use super::{auto_roles, load_api_graph, merge_roles, role_ref, validate, why, workflow_path, ROLES};
/// 角色映射按工作流文件各存一份（换文件不能沿用上一份的节点号）
pub fn roles_key(path: &str) -> String {
    format!("workflow_roles:{path}")
}

pub fn saved_roles(ctx: &Ctx, path: &str) -> Option<String> {
    crate::repo::settings::get(ctx, &roles_key(path))
}

/// 手指覆盖先按类名校验再落库：存进去一个错类名的节点，下一次提交就会往不该填的输入里写。
/// 返回（存下的映射，被拒的项）
pub fn set_saved_roles(ctx: &Ctx, path: &str, roles: &Map<String, Value>) -> (Map<String, Value>, Vec<Value>) {
    let g = load_api_graph(Path::new(path));
    let mut kept = Map::new();
    let mut rejected = Vec::new();
    for r in ROLES.iter() {
        let Some(v) = roles.get(r.key) else { continue };
        let Some(node) = v.as_str().map(str::trim) else {
            rejected.push(why("srv.wf.roleValue", json!({ "role": role_ref(r.key) })));
            continue;
        };
        if node.is_empty() {
            continue; // 清空 = 交回自动预填
        }
        match g.as_ref().and_then(|g| g.get(node)).and_then(|n| n["class_type"].as_str()) {
            Some(cls) if cls == r.class => {
                kept.insert(r.key.into(), Value::String(node.to_string()));
            }
            Some(cls) => rejected.push(why("srv.wf.roleClass", json!({ "role": role_ref(r.key), "class": r.class, "id": node, "found": cls }))),
            None => rejected.push(why("srv.wf.roleNotInGraph", json!({ "role": role_ref(r.key), "id": node }))),
        }
    }
    let key = roles_key(path);
    if kept.is_empty() {
        let _ = crate::repo::settings::del(ctx, &key);
    } else if let Ok(text) = serde_json::to_string(&Value::Object(kept.clone())) {
        let _ = crate::repo::settings::put(ctx, &key, &text);
    }
    (kept, rejected)
}

/// 一张文件 + 一份手指 → 该不该用它提交。`graph` 是 None 表示这文件根本当不了计算图
/// （读不到、或不是 API 导出），此时只能走内置图，且原因要显示出来——不做静默替换。
struct Assessment {
    graph: Option<Map<String, Value>>,
    roles: Map<String, Value>,
    errors: Vec<Value>,
    /// 当不了计算图时那句原话：钥匙给界面查，参数随钥匙走
    reason: Value,
}

fn assess(path: &str, saved: Option<&str>) -> Assessment {
    let graph = load_api_graph(Path::new(path));
    match graph {
        Some(g) => {
            let roles = merge_roles(&g, saved);
            Assessment { errors: validate(&g, &roles), graph: Some(g), roles, reason: Value::Null }
        }
        None => Assessment {
            graph: None,
            roles: Map::new(),
            errors: Vec::new(),
            reason: if Path::new(path).exists() {
                why("srv.wf.uiExport", Value::Null)
            } else {
                why("srv.wf.fileGone", json!({ "path": path.to_string() }))
            },
        },
    }
}

/// 提交用哪张计算图。**不回退**是设计的一部分（D14）：
/// 角色认不出却改用内置图，等于把"参数读自你的文件"那句谎从参数层搬到结构层。
#[derive(Debug)]
pub enum Plan {
    Mapped { graph: Map<String, Value>, profile: crate::models::workflow::Profile },
    /// 用你文件里这张图提交（已按角色校验通过）
    Workflow { graph: Map<String, Value>, roles: Map<String, Value> },
    /// 文件不是 API 导出（或读不到）：用内置图，并把原因显示出来
    Builtin { reason: Value },
    /// 是你的图，但角色/结构对不上：拒绝提交
    Refused { errors: Vec<Value> },
}

pub fn plan_at(path: &str, saved: Option<&str>) -> Plan {
    match assess(path, saved) {
        Assessment { graph: Some(g), roles, errors, .. } if errors.is_empty() => Plan::Workflow { graph: g, roles },
        Assessment { errors, .. } if !errors.is_empty() => Plan::Refused { errors },
        Assessment { reason, .. } => Plan::Builtin { reason },
    }
}

pub fn plan(ctx: &Ctx) -> Plan {
    let p = workflow_path(ctx);
    match crate::service::workflow_graph::profile(ctx, &p) {
        Ok(Some(profile)) => {
            if let Some(graph) = load_api_graph(Path::new(&p)) {
                return match crate::service::workflow_graph::validate(&graph, &profile) {
                    Ok(()) => Plan::Mapped { graph, profile },
                    Err(e) => { let (code, args) = e.reason(); Plan::Refused { errors: vec![why(&code, json!(args))] } },
                };
            }
            return Plan::Refused { errors: vec![why("srv.graph.format", Value::Null)] };
        }
        Err(e) => { let (code, args) = e.reason(); return Plan::Refused { errors: vec![why(&code, json!(args))] }; }
        Ok(None) => {}
    }
    plan_at(&p, saved_roles(ctx, &p).as_deref())
}

/// 设置里那张角色表：节点清单 + 自动预填 + 存过的手指 + 生效值 + 校验结论
pub fn roles_view(ctx: &Ctx) -> crate::error::Result<Value> {
    let p = workflow_path(ctx);
    let saved_raw = saved_roles(ctx, &p);
    let a = assess(&p, saved_raw.as_deref());
    let mut nodes: Vec<Value> = Vec::new();
    if let Some(g) = &a.graph {
        for (id, n) in g {
            nodes.push(serde_json::json!({
                "id": id,
                "class_type": n["class_type"].as_str().unwrap_or(""),
                "title": n["_meta"]["title"].as_str().unwrap_or(""),
            }));
        }
        nodes.sort_by(|x, y| (x["class_type"].as_str().unwrap_or("").to_string(), x["id"].as_str().unwrap_or("").to_string()).cmp(&(
            y["class_type"].as_str().unwrap_or("").to_string(),
            y["id"].as_str().unwrap_or("").to_string(),
        )));
    }
    let saved_map: Map<String, Value> = saved_raw
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let can_takeover = a.graph.is_some() && a.errors.is_empty();
    Ok(serde_json::json!({
        "workflow_path": p,
        "is_api": a.graph.is_some(),
        "nodes": nodes,
        "roles": ROLES.iter().map(|r| serde_json::json!({"key": r.key, "class": r.class, "label": r.label, "required": r.required})).collect::<Vec<_>>(),
        "auto": Value::Object(a.graph.as_ref().map(auto_roles).unwrap_or_default()),
        "saved": Value::Object(saved_map),
        "effective": Value::Object(a.roles),
        "errors": a.errors,
        "reason": a.reason.get("code").cloned().unwrap_or(Value::Null),
        "reason_args": a.reason.get("args").cloned().unwrap_or(Value::Null),
        "can_takeover": can_takeover,
    }))
}
