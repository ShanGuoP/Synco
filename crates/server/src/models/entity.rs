//! 表行的类型化视图。列的读取仍走 `repo` 的动态映射（与 Node 的 `{...row}` 对齐），
//! 这里只把"哪一列是什么类型"固定下来，免得 handler 里到处 `get("x").and_then(as_i64)`。

use serde_json::Value;

fn own(v: Option<&Value>) -> Option<String> {
    v.and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
}
fn text(v: Option<&Value>) -> String {
    own(v).unwrap_or_default()
}
fn int(v: Option<&Value>) -> i64 {
    v.and_then(|x| x.as_i64()).unwrap_or(0)
}
fn opt_int(v: Option<&Value>) -> Option<i64> {
    v.and_then(|x| x.as_i64()).filter(|x| *x != 0)
}

#[derive(Clone, Debug)]
pub struct Image {
    pub id: i64,
    pub project_id: i64,
    pub name: String,
    pub orig_path: String,
    pub mask_path: Option<String>,
    /// M3 的派生档；缺失时前端一律回退 orig_url，不白屏
    pub thumb_path: Option<String>,
    pub proxy_path: Option<String>,
    pub w: i64,
    pub h: i64,
    pub created_at: Option<String>,
    /// `photo` = 导入的照片，`sketch` = 画布里的画稿。
    /// 画稿也有 orig_path（它就是那张画稿的 PNG），但**不能**进修图那条链路：
    /// 没有"蒙版外要保持"这回事，裁切与缝合对它没有意义。路由全靠这一列判。
    pub kind: String,
    /// 「另存为新图」造出来的子图指向的父图与来源结果行。
    /// 以前这份信息只在文件名里（`… 派生{结果号}.png`），改个名就断，
    /// 所以项目页的派生入口要靠这两列，而不是靠猜字符串。
    pub derived_from: Option<i64>,
    pub derived_result: Option<i64>,
}

impl Image {
    pub fn from_value(v: &Value) -> Self {
        Self {
            id: int(v.get("id")),
            project_id: int(v.get("project_id")),
            name: text(v.get("name")),
            orig_path: text(v.get("orig_path")),
            mask_path: own(v.get("mask_path")),
            thumb_path: own(v.get("thumb_path")),
            proxy_path: own(v.get("proxy_path")),
            w: int(v.get("w")),
            h: int(v.get("h")),
            created_at: own(v.get("created_at")),
            kind: own(v.get("kind")).unwrap_or_else(|| "photo".into()),
            derived_from: opt_int(v.get("derived_from")),
            derived_result: opt_int(v.get("derived_result")),
        }
    }

    pub fn has_mask(&self) -> bool {
        self.mask_path.is_some()
    }

    pub fn is_sketch(&self) -> bool {
        self.kind == "sketch"
    }
}

/// 结果行的可读视图。只列**代码要用**的列：展示用的字段由 `dto::result_json` 直接从
/// SELECT 出来的 Value 展平（Node 那边就是 `{...row}`，键集合跟着接口走），
/// 在这里再抄一份只会让两边开始漂移。
#[derive(Clone, Debug)]
pub struct ResultRow {
    pub id: i64,
    pub image_id: i64,
    pub project_id: i64,
    pub status: String,
    pub prompt_id: Option<String>,
    pub prompt: String,
    pub final_path: Option<String>,
    pub crop_path: Option<String>,
    pub maskoverlay_path: Option<String>,
    pub thumb_path: Option<String>,
    /// 画布这一版提交时的线稿快照（照片那一路没有这一项）
    pub sketch_path: Option<String>,
    pub backend: String,
    /// 提交那一刻的参数快照。云端队列按这一行的 `edge`/`invert` 出图，
    /// 而不是按跑到的那一刻的全局设置——设置改在两张中间不该让后一张变样。
    pub settings_json: Option<String>,
    /// 无损重算的原料：模型回来的窗口原图、这一枪实际用的那份遮罩、当时的缝合与调整参数。
    /// 缺任一件，这一行就只能给成图那一档（0.3.1 以前的行本来就存无损 PNG，不需要重算）
    pub raw_path: Option<String>,
    pub mask_snap_path: Option<String>,
    pub snap_json: Option<String>,
}

impl ResultRow {
    pub fn from_value(v: &Value) -> Self {
        Self {
            id: int(v.get("id")),
            image_id: int(v.get("image_id")),
            project_id: int(v.get("project_id")),
            status: text(v.get("status")),
            prompt_id: own(v.get("prompt_id")),
            prompt: text(v.get("prompt")),
            final_path: own(v.get("final_path")),
            crop_path: own(v.get("crop_path")),
            maskoverlay_path: own(v.get("maskoverlay_path")),
            thumb_path: own(v.get("thumb_path")),
            sketch_path: own(v.get("sketch_path")),
            // 老行没这一列时按本机链路解释，历史云端结果才不会被拿步数/种子去说明
            backend: own(v.get("backend")).unwrap_or_else(|| "comfyui".into()),
            settings_json: v.get("settings_json").and_then(|x| x.as_str()).map(str::to_string),
            raw_path: own(v.get("raw_path")),
            mask_snap_path: own(v.get("mask_snap_path")),
            snap_json: v.get("snap_json").and_then(|x| x.as_str()).map(str::to_string),
        }
    }

    /// 无损重算的参数快照；坏 JSON 与没存过都当 `Null`，调用方按"重算不成"处理
    pub fn snap(&self) -> Value {
        self.snap_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or(Value::Null)
    }

    /// 这一行提交时的参数快照；坏 JSON 与没存过都当空对象
    pub fn settings(&self) -> Value {
        self.settings_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
    }

    /// 云端行没有 prompt_id：本机那套轮询接不回来
    pub fn is_cloud(&self) -> bool {
        self.backend == "cloud"
    }
    pub fn running(&self) -> bool {
        self.status == "running"
    }
}
