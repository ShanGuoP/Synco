#![cfg(test)]
//! 模块行为与边界回归。
use super::*;

/// 清单比对钥匙而不是句子：措辞改了不该让单测红
fn codes(list: &[Value]) -> Vec<String> {
    list.iter().map(|e| e["code"].as_str().unwrap_or("").to_string()).collect()
}

fn tmp_json(name: &str, body: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("synco-wf-{name}-{}.json", std::process::id()));
    std::fs::write(&p, body).unwrap();
    p
}

/// UI/litegraph 导出：只有 widgets_values 的位置，没有名字
const LITE: &str = r#"{"nodes":[
  {"id":3,"type":"UNETLoader","mode":0,"widgets_values":["qwen_bf16.safetensors","default"]},
  {"id":4,"type":"LoraLoaderModelOnly","mode":0,"widgets_values":["loraA.safetensors",0.8],"inputs":[{"type":"MODEL","link":11}]},
  {"id":5,"type":"KSampler","mode":0,"widgets_values":["123","randomize",20,3.5,"euler","normal",true]},
  {"id":6,"type":"TextEncodeQwenImage21","mode":0,"widgets_values":["默认正向","默认负面"]},
  {"id":8,"type":"CLIPLoader","mode":0,"widgets_values":["qwen3vl.safetensors","qwen"]},
  {"id":9,"type":"VAELoader","mode":0,"widgets_values":["qwen_vae.safetensors"]}
],"links":[[11,3,0,4,1,"MODEL"]]}"#;

/// API 导出：inputs 按名字给值，`["节点号", 引脚]` 是连接
const API: &str = r#"{
  "1":{"class_type":"LoadImage","inputs":{"image":"a.png"}},
  "3":{"class_type":"UNETLoader","inputs":{"unet_name":"qwen_bf16.safetensors","weight_dtype":"default"}},
  "4":{"class_type":"LoraLoaderModelOnly","inputs":{"model":["3",0],"lora_name":"loraA.safetensors","strength_model":0.7}},
  "7":{"class_type":"LoraLoaderModelOnly","inputs":{"model":["4",0],"lora_name":"loraB.safetensors","strength_model":0.4}},
  "8":{"class_type":"CLIPLoader","inputs":{"clip_name":"qwen3vl.safetensors","type":"qwen"}},
  "9":{"class_type":"VAELoader","inputs":{"vae_name":"qwen_vae.safetensors"}},
  "11":{"class_type":"TextEncodeQwenImage21","inputs":{"prompt":"写成 API 形态","negative_prompt":"不要的东西"}},
  "14":{"class_type":"KSampler","inputs":{"model":["7",0],"seed":1,"steps":28,"cfg":4.5,"sampler_name":"dpmpp_2m","scheduler":"karras","denoise":1}},
  "10":{"class_type":"InpaintCropImproved","inputs":{"image":["1",0],"mask":["2",0],"mask_expand_pixels":64,"device_mode":"auto"}}
}"#;

#[test]
fn ui_导出按位置读得出关键参数() {
    let p = tmp_json("lite", LITE);
    let (m, fmt) = load_map(&p).unwrap();
    assert_eq!(fmt, "litegraph");
    assert_eq!(m["unet"], Value::String("qwen_bf16.safetensors".into()));
    assert_eq!(m["steps"], Value::from(20));
    assert_eq!(m["cfg"], Value::from(3.5));
    assert_eq!(m["sampler"], Value::String("euler".into()));
    assert_eq!(m["negative"], Value::String("默认负面".into()));
    let loras = m["loras"].as_array().unwrap();
    assert_eq!(loras.len(), 1);
    assert_eq!(loras[0]["name"], Value::String("loraA.safetensors".into()));
    std::fs::remove_file(p).ok();
}

/// API 导出的 `inputs` 是按名字的：节点顺序、控件个数都不再能让参数错位
#[test]
fn api_导出按名字读参数与裁切() {
    let p = tmp_json("api", API);
    let (m, fmt) = load_map(&p).unwrap();
    assert_eq!(fmt, "api");
    assert_eq!(m["steps"], Value::from(28));
    assert_eq!(m["cfg"], Value::from(4.5));
    assert_eq!(m["sampler"], Value::String("dpmpp_2m".into()));
    assert_eq!(m["scheduler"], Value::String("karras".into()));
    assert_eq!(m["clip"], Value::String("qwen3vl.safetensors".into()));
    assert_eq!(m["clip_type"], Value::String("qwen".into()));
    assert_eq!(m["negative"], Value::String("不要的东西".into()));
    // 链是 3 → 4 → 7，顺序不能反（先挂的在前）
    let loras = m["loras"].as_array().unwrap();
    assert_eq!(loras.iter().map(|l| l["name"].as_str().unwrap().to_string()).collect::<Vec<_>>(), vec!["loraA.safetensors", "loraB.safetensors"]);
    assert_eq!(loras[1]["strength"], Value::from(0.4));
    // 裁切按名给值，且连接引用（数组形态）不会被当成参数值
    let ci = m["crop_inputs"].as_object().unwrap();
    assert_eq!(ci["mask_expand_pixels"], Value::from(64));
    assert_eq!(ci["device_mode"], Value::String("auto".into()));
    assert!(!ci.contains_key("image"), "连接引用不该被当成分支参数");
    std::fs::remove_file(p).ok();
}

#[test]
fn api_导出里停用的_lora_保持停用() {
    let body = API.replace("\"7\":{\"class_type\":\"LoraLoaderModelOnly\"", "\"7\":{\"enabled\":0,\"class_type\":\"LoraLoaderModelOnly\"");
    let p = tmp_json("off", &body);
    let (m, _) = load_map(&p).unwrap();
    let loras = m["loras"].as_array().unwrap();
    assert_eq!(loras[1]["enabled"], Value::Bool(false));
    assert_eq!(loras[0]["enabled"], Value::Bool(true));
    std::fs::remove_file(p).ok();
}

/// 合并/手改过的 json 里两个 LoRA 可以有同一个 id：旧写法在这里会无限 push
#[test]
fn 环形_lora_链被_visited_与深度上限夹住() {
    let lite = r#"{"nodes":[
      {"id":3,"type":"UNETLoader","mode":0,"widgets_values":["u.safetensors","default"]},
      {"id":7,"type":"LoraLoaderModelOnly","mode":0,"widgets_values":["A.safetensors",1],"inputs":[{"type":"MODEL","link":11}]},
      {"id":7,"type":"LoraLoaderModelOnly","mode":0,"widgets_values":["B.safetensors",1],"inputs":[{"type":"MODEL","link":12}]}
    ],"links":[[11,3,0,7,1,"MODEL"],[12,7,0,7,1,"MODEL"]]}"#;
    let p = tmp_json("ring", lite);
    let (m, _) = load_map(&p).unwrap();
    let n = m["loras"].as_array().unwrap().len();
    assert!(n <= LORA_MAX, "LoRA 链没被上限夹住：{n}");
    std::fs::remove_file(p).ok();
}

#[test]
fn 认不出的格式明确失败而不是回空表() {
    for body in ["{}", r#"{"foo":{"bar":1}}"#, r#"{"nodes":{}}"#] {
        let p = tmp_json("junk", body);
        assert!(load_map(&p).is_err(), "{body} 应该被拒");
        std::fs::remove_file(p).ok();
    }
}

#[test]
fn 清点数得出节点与缝合那一对() {
    let p = tmp_json("insp", API);
    let v = inspect(&p).unwrap();
    assert_eq!(v["format"], Value::String("api".into()));
    assert_eq!(v["total"], Value::from(9));
    // 这个样本有 InpaintCropImproved 但没有 InpaintStitchImproved —— 缺一个就不能算"能缝合"
    assert_eq!(v["stitch_pair_ok"], Value::Bool(false));
    assert!(v["known_missing"].as_array().unwrap().iter().any(|x| x == "InpaintStitchImproved"));
    assert!(v["nodes"].as_array().unwrap().iter().any(|n| n["class_type"] == "KSampler"));
    std::fs::remove_file(p).ok();
}

#[test]
fn 坏文件不抛只回错误串() {
    let p = tmp_json("noperm", "not json at all");
    assert!(load_map(&p).unwrap_err().text().contains("合法 JSON"));
    assert!(inspect(&p).is_err());
    std::fs::remove_file(p).ok();
}

/// 一张能接管提交的最小图：形状照 ComfyUI 的 API 导出，节点号刻意不是内置图那套
const WF: &str = r#"{
  "1":{"class_type":"LoadImage","inputs":{"image":"占位.png"}},
  "2":{"class_type":"LoadImageMask","inputs":{"image":"占位m.png","channel":"red"}},
  "3":{"class_type":"UNETLoader","inputs":{"unet_name":"u.safetensors"}},
  "4":{"class_type":"LoraLoaderModelOnly","inputs":{"model":["3",0],"lora_name":"A.safetensors","strength_model":0.9}},
  "5":{"class_type":"LoraLoaderModelOnly","inputs":{"model":["4",0],"lora_name":"B.safetensors","strength_model":0.5}},
  "6":{"class_type":"CLIPLoader","inputs":{"clip_name":"c.safetensors"}},
  "7":{"class_type":"VAELoader","inputs":{"vae_name":"v.safetensors"}},
  "10":{"class_type":"InpaintCropImproved","inputs":{"image":["1",0],"mask":["2",0],"mask_expand_pixels":64}},
  "11":{"class_type":"TextEncodeQwenImage21","inputs":{"clip":["6",0],"vae":["7",0],"prompt":"文件里的正向","negative_prompt":"文件里的负面","images.image_1":["10",1]}},
  "12":{"class_type":"VAEEncode","inputs":{"pixels":["10",1],"vae":["7",0]}},
  "13":{"class_type":"SetLatentNoiseMask","inputs":{"samples":["12",0],"mask":["10",2]}},
  "14":{"class_type":"KSampler","inputs":{"model":["5",0],"positive":["11",0],"negative":["11",1],"latent_image":["13",0],"seed":7,"steps":18,"cfg":2.5,"sampler_name":"euler","scheduler":"simple","denoise":1}},
  "15":{"class_type":"VAEDecode","inputs":{"samples":["14",0],"vae":["7",0]}},
  "16":{"class_type":"InpaintStitchImproved","inputs":{"stitcher":["10",0],"inpainted_image":["15",0]}},
  "17":{"class_type":"SaveImage","inputs":{"images":["16",0],"filename_prefix":"Mine"}},
  "18":{"class_type":"DrawMaskOnImage","inputs":{"image":["10",1],"mask":["10",2]}},
  "19":{"class_type":"PreviewImage","inputs":{"images":["10",1]}},
  "20":{"class_type":"PreviewImage","inputs":{"images":["18",0]}},
  "99":{"class_type":"SaveImage","inputs":{"images":["14",0],"filename_prefix":"别的图"}}
}"#;

fn wf() -> Map<String, Value> {
    serde_json::from_str::<Value>(WF).unwrap().as_object().cloned().unwrap()
}

/// `{"prompt":{…}}` 这种把 /prompt 请求体存下来的形态也认
#[test]
fn prompt_包裹形态也当_api_导出() {
    let p = tmp_json("wrapped", &format!(r#"{{"prompt":{WF}}}"#));
    let g = load_api_graph(&p).expect("应该认出被 prompt 包起来的节点表");
    assert_eq!(g["17"]["class_type"], Value::String("SaveImage".into()));
    let (m, fmt) = load_map(&p).unwrap();
    assert_eq!(fmt, "api");
    assert_eq!(m["steps"], Value::from(18));
    std::fs::remove_file(p).ok();
}

#[test]
fn 角色按类名自动认出_输出走缝合那一路() {
    let g = wf();
    let r = auto_roles(&g);
    assert_eq!(r["load_image"], Value::String("1".into()));
    assert_eq!(r["crop"], Value::String("10".into()));
    assert_eq!(r["stitch"], Value::String("16".into()));
    // 99 号那个 SaveImage 存的是采样结果，不是缝合结果——成图角色不能挑它
    assert_eq!(r["out_final"], Value::String("17".into()));
    assert_eq!(r["out_crop"], Value::String("19".into()));
    assert_eq!(r["out_overlay"], Value::String("20".into()));
    assert!(validate(&g, &r).is_empty(), "{:?}", validate(&g, &r));
}

/// 缝合没接在裁切后面 = 回来的图不会被贴回原图；成图直接存采样结果 = 整图重绘。
/// 两种都不会报错、画面也都正常，语义却已经变了，所以必须在提交前拦住。
/// 图里没有预览节点时，可选角色必须留空。把"输出裁切区"顺手指到成图那个 SaveImage 上，
/// 读回来的就是一张与成图重复的文件——下载按钮写着"裁切图"，点开是成图。
#[test]
fn 没有预览节点时可选角色留空() {
    let mut g = wf();
    for id in ["18", "19", "20"] {
        g.remove(id);
    }
    let r = auto_roles(&g);
    assert_eq!(r["out_final"], Value::String("17".into()));
    assert!(!r.contains_key("out_crop"), "裁切区没节点可认就该空着");
    assert!(!r.contains_key("out_overlay"));
    assert!(validate(&g, &r).is_empty(), "{:?}", validate(&g, &r));

    // 手工把两路输出指到同一个节点也要拦
    let dup = merge_roles(&g, Some(r#"{"out_crop":"17"}"#));
    let errs = validate(&g, &dup);
    assert!(codes(&errs).contains(&"srv.wf.sameNode".into()), "{errs:?}");
}

#[test]
fn 接错线的图被拒绝接管() {
    let mut g = wf();
    // 裁切经提示词编码喂到采样器，所以"接错线"要接一个与这条链无关的源才测得出来
    g.insert("77".into(), serde_json::json!({"class_type":"LoadImage","inputs":{"image":"另一张.png"}}));
    g.insert("16".into(), serde_json::json!({"class_type":"InpaintStitchImproved","inputs":{"stitcher":["77",0],"inpainted_image":["77",1]}}));
    let errs = validate(&g, &auto_roles(&g));
    assert!(codes(&errs).contains(&"srv.wf.stitchOrder".into()), "{errs:?}");

    // 99 号那个 SaveImage 存的是没缝回去的采样结果：手指到它头上就得拦住
    let g2 = wf();
    let roles = merge_roles(&g2, Some(r#"{"out_final":"99"}"#));
    let errs2 = validate(&g2, &roles);
    assert!(codes(&errs2).contains(&"srv.wf.outOfStitch".into()), "{errs2:?}");
}

#[test]
fn 缺缝合节点就硬失败而不是回退() {
    let mut g = wf();
    g.remove("16");
    let p = tmp_json("nostitch", &Value::Object(g).to_string());
    match plan_at(&p.to_string_lossy(), None) {
        Plan::Refused { errors } => {
            assert!(errors.iter().any(|e| e["code"] == "srv.wf.roleMissing" && e["args"]["role"]["code"] == "wf.role.stitch"), "{errors:?}")
        }
        other => panic!("缺必需角色应该拒绝，拿到的是 {other:?}"),
    }
    std::fs::remove_file(p).ok();
}

/// UI 导出的文件当不了计算图：走内置图，但要把原因带出去（不做静默替换）
#[test]
fn ui_导出走内置图并把原因带出来() {
    let p = tmp_json("uilite", LITE);
    match plan_at(&p.to_string_lossy(), None) {
        Plan::Builtin { reason } => assert_eq!(reason["code"], "srv.wf.uiExport", "{reason}"),
        other => panic!("UI 导出应该是 Builtin，拿到 {other:?}"),
    }
    std::fs::remove_file(p).ok();
}

/// 手指过的节点号只在同一种类里收；指错类名的必须当场拒，不然提交时往不该填的输入里写值
#[test]
fn 角色映射的手指覆盖按类名收() {
    let mut g = wf();
    // 把成图角色改成手动指到 99 号（那是"直接存采样结果"那一路）：类名对得上，但结构校验要拦
    let roles = merge_roles(&g, Some(r#"{"out_final":"99"}"#));
    assert_eq!(roles["out_final"], Value::String("99".into()));
    let errs = validate(&g, &roles);
    assert!(codes(&errs).contains(&"srv.wf.outOfStitch".into()), "{errs:?}");
    // 指到不存在的节点：merge_roles 照收（它是手指的入口），由 validate 判死
    let roles2 = merge_roles(&g, Some(r#"{"out_final":"777"}"#));
    let errs2 = validate(&g, &roles2);
    assert!(errs2.iter().any(|e| e["code"] == "srv.wf.roleNotInGraph" && e["args"]["id"] == "777"), "{errs2:?}");
    // 指错类名（把采样器指到 LoadImage 上）：注入用的输入名根本不存在
    let roles3 = merge_roles(&g, Some(r#"{"ksampler":"1"}"#));
    let errs3 = validate(&g, &roles3);
    assert!(errs3.iter().any(|e| e["code"] == "srv.wf.roleNoInput" && e["args"]["input"] == "seed"), "{errs3:?}");
    // 空串 = 交回自动认出；这里 17 已被摘掉，所以那个键干脆不存在
    g.remove("17");
    g.remove("99");
    let roles4 = merge_roles(&g, Some(r#"{"out_final":""}"#));
    assert!(!roles4.contains_key("out_final"), "清空后不该留这个键");
}

#[test]
fn 有环的图在提交前就说清() {
    let mut g = wf();
    g.insert("14".into(), serde_json::json!({"class_type":"KSampler","inputs":{"model":["14",0],"positive":["11",0],"negative":["11",1],"latent_image":["13",0],"seed":7,"steps":18,"cfg":2.5}}));
    assert!(has_cycle(&g));
    let errs = validate(&g, &auto_roles(&g));
    assert!(codes(&errs).contains(&"srv.wf.cycle".into()), "{errs:?}");
}

#[test]
fn feeds_认间接连线也不转死在菱形里() {
    let g = wf();
    assert!(feeds(&g, "1", "10"), "直连");
    assert!(feeds(&g, "3", "14"), "要穿两条 LoRA");
    assert!(!feeds(&g, "17", "14"), "下游不该反过来喂上游");
    assert!(!feeds(&g, "1", "1"), "自己不算喂给自己");
}
