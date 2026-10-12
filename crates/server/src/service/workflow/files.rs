//! 工作流文件读取与检查，文件错误在此转换为服务错误。
use std::path::Path;
use crate::error::AppError;
use serde_json::{Map, Value};
use super::parse::{api_nodes, detect, read_api, read_litegraph};
use super::KNOWN_CLASSES;
/// 读并解析一个工作流文件，返回（参数表，格式）。
/// 认不出格式时给的是**明确失败**而不是空表——以前 API 导出会解析成"零项"，
/// 而 cfgSource 照样标成 "workflow"，等于谎称参数读自你的文件。
pub fn load_map(p: &Path) -> crate::error::Result<(Map<String, Value>, &'static str)> {
    let text = std::fs::read_to_string(p).map_err(|e| AppError::detail("srv.wf.readFail", e))?;
    let d: Value = serde_json::from_str(&text).map_err(|e| AppError::detail("srv.wf.badJson", e))?;
    let fmt = detect(&d).ok_or_else(|| AppError::bad("srv.wf.unknownFormat"))?;
    let mut m = Map::new();
    match api_nodes(&d) {
        Some(nodes) if fmt == "api" => read_api(nodes, &mut m),
        _ => read_litegraph(&d, &mut m),
    }
    Ok((m, fmt))
}

/// 读一个 API 导出的工作流，返回 `{节点号: {class_type, inputs}}`。不是 API 导出时给 None。
pub fn load_api_graph(p: &Path) -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(p).ok()?;
    let d: Value = serde_json::from_str(&text).ok()?;
    api_nodes(&d).cloned()
}

/// 把文件里的节点摊平成可比对的 `(节点号, 类名, 输入名)` 列表，两种格式同一套出口
pub fn inspect(p: &Path) -> crate::error::Result<Value> {
    let text = std::fs::read_to_string(p).map_err(|e| AppError::detail("srv.wf.readFail", e))?;
    let d: Value = serde_json::from_str(&text).map_err(|e| AppError::detail("srv.wf.badJson", e))?;
    let fmt = detect(&d).ok_or_else(|| AppError::bad("srv.wf.unknownFormatShort"))?;
    let mut rows: Vec<Value> = Vec::new();
    if fmt == "api" {
        if let Some(o) = api_nodes(&d) {
            for (id, n) in o {
                rows.push(serde_json::json!({
                    "id": id,
                    "class_type": n["class_type"].as_str().unwrap_or(""),
                    "inputs": n["inputs"].as_object().map(|i| i.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
                    "enabled": match &n["enabled"] { Value::Bool(b) => Value::Bool(*b), Value::Number(x) => Value::Bool(x.as_f64().unwrap_or(1.0) != 0.0), _ => Value::Bool(true) },
                }));
            }
        }
    } else if let Some(arr) = d["nodes"].as_array() {
        for n in arr {
            rows.push(serde_json::json!({
                "id": n["id"].to_string(),
                "class_type": n["type"].as_str().unwrap_or(""),
                "inputs": n["widgets_values"].as_array().map(|a| a.iter().enumerate().map(|(i, _)| format!("#{i}")).collect::<Vec<_>>()).unwrap_or_default(),
                "enabled": n["mode"].as_i64().unwrap_or(0) == 0,
            }));
        }
    }
    rows.sort_by(|a, b| a["class_type"].as_str().unwrap_or("").cmp(b["class_type"].as_str().unwrap_or("")));
    let present: Vec<&str> = KNOWN_CLASSES
        .iter()
        .filter(|c| rows.iter().any(|r| r["class_type"].as_str() == Some(*c)))
        .copied()
        .collect();
    let missing: Vec<&str> = KNOWN_CLASSES.iter().filter(|c| !present.contains(c)).copied().collect();
    // 缝合那一对是"蒙版外逐像素不动"的全部依赖，缺了它回来的就是一张重绘过的整图
    let pair = ["InpaintCropImproved", "InpaintStitchImproved"];
    let stitch_pair_ok = pair.iter().all(|c| present.contains(c));
    Ok(serde_json::json!({
        "format": fmt,
        "total": rows.len(),
        "nodes": rows,
        "known_present": present,
        "known_missing": missing,
        "stitch_pair_ok": stitch_pair_ok,
    }))
}
