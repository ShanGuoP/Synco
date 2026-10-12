#![cfg(test)]
//! 模块行为与边界回归。
use super::{inject_workflow, sample_args, Outputs};
use serde_json::{json, Map, Value};

fn table(s: &str) -> Map<String, Value> {
    serde_json::from_str::<Value>(s).unwrap().as_object().cloned().unwrap()
}

/// 用户导出的图：节点号与内置图那套完全不同，两条 LoRA 串在 unet 之后
const USER: &str = r#"{
  "21":{"class_type":"LoadImage","inputs":{"image":"占位.png"}},
  "22":{"class_type":"LoadImageMask","inputs":{"image":"占位m.png","channel":"red"}},
  "30":{"class_type":"UNETLoader","inputs":{"unet_name":"u.safetensors"}},
  "31":{"class_type":"LoraLoaderModelOnly","inputs":{"model":["30",0],"lora_name":"A.safetensors","strength_model":0.9}},
  "32":{"class_type":"LoraLoaderModelOnly","inputs":{"model":["31",0],"lora_name":"B.safetensors","strength_model":0.5}},
  "40":{"class_type":"InpaintCropImproved","inputs":{"image":["21",0],"mask":["22",0],"mask_expand_pixels":64,"device_mode":"cpu (compatible)"}},
  "41":{"class_type":"TextEncodeQwenImage21","inputs":{"clip":["42",0],"prompt":"文件里的正向","negative_prompt":"文件里的负面"}},
  "43":{"class_type":"KSampler","inputs":{"model":["32",0],"positive":["41",0],"seed":7,"steps":18,"cfg":2.5,"sampler_name":"euler","scheduler":"simple"}},
  "50":{"class_type":"InpaintStitchImproved","inputs":{"stitcher":["40",0],"inpainted_image":["43",0]}},
  "51":{"class_type":"SaveImage","inputs":{"images":["50",0],"filename_prefix":"Mine"}}
}"#;

const ROLES: &str = r#"{
  "load_image":"21","load_mask":"22","text_encode":"41","ksampler":"43",
  "crop":"40","stitch":"50","out_final":"51","out_crop":"52","out_overlay":"53"
}"#;

fn settings() -> Value {
    json!({
        "prompt": "把妆容修干净", "negative": "不要的东西", "steps": 26, "cfg": 3.5, "seed": 4242,
        "loras": [
            { "name": "A.safetensors", "strength": 0.7, "enabled": false },
            { "name": "B.safetensors", "strength": 1.0, "enabled": true }
        ]
    })
}

/// 接管提交只填这几处：其余（裁切参数、模型名、采样器选型）一概按用户文件里那样跑
#[test]
fn 注入只动该动的输入() {
    let g = table(USER);
    let roles = table(ROLES);
    let out = inject_workflow(&g, &roles, "myphoto.png", "mymask.png", &settings(), 991).unwrap();
    assert_eq!(out["21"]["inputs"]["image"], json!("myphoto.png"));
    assert_eq!(out["22"]["inputs"]["image"], json!("mymask.png"));
    assert_eq!(out["41"]["inputs"]["prompt"], json!("把妆容修干净"));
    assert_eq!(out["41"]["inputs"]["negative_prompt"], json!("不要的东西"));
    assert_eq!(out["43"]["inputs"]["seed"], json!(991), "种子由调用方算好（带图片偏移）");
    assert_eq!(out["43"]["inputs"]["steps"], json!(26));
    assert_eq!(out["43"]["inputs"]["cfg"], json!(3.5));
    // 没让改的东西原样留着
    assert_eq!(out["43"]["inputs"]["sampler_name"], json!("euler"));
    assert_eq!(out["40"]["inputs"]["mask_expand_pixels"], json!(64));
    assert_eq!(out["30"]["inputs"]["unet_name"], json!("u.safetensors"));
    // 传进去的表不能被改坏（下一次提交还要用同一份）
    assert_eq!(g["21"]["inputs"]["image"], json!("占位.png"));
}

/// 面板上关了 A：A 节点从图里摘掉，B 接到 unet 上。
/// 只加 `enabled:false` 是不够的——老版本 ComfyUI 不认那个键，会照跑。
#[test]
fn 关掉的_lora_被摘掉且下游接回上游() {
    let g = table(USER);
    let roles = table(ROLES);
    let out = inject_workflow(&g, &roles, "p", "m", &settings(), 1).unwrap();
    assert!(out.get("31").is_none(), "关掉的 LoRA 节点不该留在图里");
    assert_eq!(out["32"]["inputs"]["model"], json!(["30", 0]), "B 要接回 unet");
    assert_eq!(out["32"]["inputs"]["strength_model"], json!(1.0));
    assert_eq!(out["43"]["inputs"]["model"], json!(["32", 0]));
}

#[test]
fn 两条都关时采样器直接接_unet() {
    let mut s = settings();
    s["loras"][0]["enabled"] = json!(true);
    s["loras"][1]["enabled"] = json!(false);
    let out = inject_workflow(&table(USER), &table(ROLES), "p", "m", &s, 1).unwrap();
    assert_eq!(out["31"]["inputs"]["strength_model"], json!(0.7), "开着的按面板强度写");
    assert!(out.get("32").is_none());
    assert_eq!(out["43"]["inputs"]["model"], json!(["31", 0]));

    let mut s2 = settings();
    s2["loras"][0]["enabled"] = json!(false);
    s2["loras"][1]["enabled"] = json!(false);
    let out2 = inject_workflow(&table(USER), &table(ROLES), "p", "m", &s2, 1).unwrap();
    assert!(out2.get("31").is_none() && out2.get("32").is_none(), "两条都关就都不在图里");
    assert_eq!(out2["43"]["inputs"]["model"], json!(["30", 0]));
}

/// 面板里的 LoRA 清单与工作流对不上时（挂在 CLIP 上、或换了文件）：不碰它，
/// 免得把一条 Synco 不认识的链给摘断
#[test]
fn 面板没有的_lora_不动() {
    let mut s = settings();
    s["loras"] = json!([{ "name": "根本不存在的.safetensors", "strength": 1, "enabled": false }]);
    let out = inject_workflow(&table(USER), &table(ROLES), "p", "m", &s, 1).unwrap();
    assert!(out.get("31").is_some() && out.get("32").is_some());
}

#[test]
fn 角色指的节点不在图里就报错而不是硬填() {
    let g = table(USER);
    let mut roles = table(ROLES);
    roles.remove("load_mask");
    let e = inject_workflow(&g, &roles, "p", "m", &settings(), 1).unwrap_err().text();
    assert!(e.contains("load_mask"), "{e}");
    let roles2 = table(r#"{"load_image":"999","load_mask":"22","text_encode":"41","ksampler":"43"}"#);
    let e2 = inject_workflow(&g, &roles2, "p", "m", &settings(), 1).unwrap_err().text();
    assert!(e2.contains("999"), "{e2}");
}

/// 负面词为空时不清空文件里那句：用户在工作流里写好的负面提示词不该被界面抹掉
#[test]
fn 空负面词不覆盖文件里的那句() {
    let mut s = settings();
    s["negative"] = json!("");
    let out = inject_workflow(&table(USER), &table(ROLES), "p", "m", &s, 1).unwrap();
    assert_eq!(out["41"]["inputs"]["negative_prompt"], json!("文件里的负面"));
}

#[test]
fn 输出节点号按行取_老行按内置解释() {
    assert_eq!(Outputs::builtin().pairs(), vec![("17".into(), "final"), ("19".into(), "crop"), ("20".into(), "maskoverlay")]);
    // 改造前写的行没有 workflow_out：还按 17/19/20 读
    let old = Outputs::from_settings(&json!({ "steps": 20 }));
    assert_eq!(old.final_id.as_deref(), Some("17"));
    assert_eq!(old.required_tags(), vec!["final", "crop", "maskoverlay"]);

    let row = Outputs::from_settings(&json!({ "workflow_out": { "final": "51", "crop": "52" } }));
    assert_eq!(row.final_id.as_deref(), Some("51"));
    assert_eq!(row.overlay_id, None, "这一行没有遮罩预览那一路");
    assert_eq!(row.required_tags(), vec!["final", "crop"], "没映射的输出不能拿来判失败");
    // 落盘 → 读回要对得上
    let back = Outputs::from_settings(&json!({ "workflow_out": row.to_json() }));
    assert_eq!(back.final_id, row.final_id);
    assert_eq!(back.crop_id, row.crop_id);
}

#[test]
fn 合法采样参数按面板区间放行() {
    assert_eq!(sample_args(&json!({ "steps": 20, "cfg": 3 })).unwrap(), (20, 3.0));
    assert_eq!(sample_args(&json!({ "steps": "4", "cfg": "0.5" })).unwrap(), (4, 0.5));
    assert_eq!(sample_args(&json!({ "steps": 20.6, "cfg": 3 })).unwrap(), (21, 3.0), "步数取整");
}

/// M4 的回归：云端行是 steps=0/cfg=0 的形状，串回本机时不能把 0 送进 KSampler
#[test]
fn 云端形状的零零参数被挡在提交之外() {
    for bad in [json!({ "steps": 0, "cfg": 0 }), json!({ "steps": -3, "cfg": 3 }), json!({ "cfg": 3 }), json!({ "steps": 20 })] {
        assert!(sample_args(&bad).is_err(), "{bad} 不该被接受");
    }
    assert!(sample_args(&json!({ "steps": 999, "cfg": 3 })).is_err());
    assert!(sample_args(&json!({ "steps": 20, "cfg": 99 })).is_err());
    let e = sample_args(&json!({ "steps": 0, "cfg": 0 })).unwrap_err().text();
    assert!(e.contains("步数"), "报错要指名是哪一项：{e}");
}
