# Synco Photo

应用显示名称为 **Synco Photo**；源码包名、可执行文件名、`SYNCO_*` 环境变量、数据目录和互联注册表项保留原名称，以兼容现有数据与集成。

简体中文 · **[English](README.en.md)**

**只重绘你涂到的地方，其余像素不动。**

导入照片 → 涂出要修的区域 → 交给本机 ComfyUI 或云端图像接口重绘 → 自动缝回原图。

和"整图重绘"的区别：整图重绘会顺手改掉你的背景、衣服纹理和没让你动的脸。Synco Photo 只替换蒙版里的像素，羽化过渡带之外与原图**逐位相同**，接缝用多频段融合过渡，不留贴图边框。

这个Demo最初很小，只是一个 Node 写的工具，功能栏里也只有一个按钮**GPT Image 重绘**，后来要接 ComfyUI，又发现原图处理有性能瓶颈，旧的那点代码撑不住了（图片滑动经常卡死），想改，改不动，毕竟现在我的代码水平，也就写一手 CRUD ，索性推倒重来，方案与大肥鱼DeepSeek-v4.1-Flash、GLM-5.3、GPT-6.1-Sol来来回回讨论了几轮，最后定了 Rust重构的方案，其实本来打算用更为熟悉的Go，但是与AI对线的时候发现Rust在图像处理方面的性能似乎更好。

行，那就 Rust。

Rust 版的绝大多数代码是 Qwen-3.8-Flash和GPT-6.1-Sol写的，最后GLM-5.3、DeepSeek-v4.1-Flash 和 GPT-6.1-Sol 负责交叉审计。

---

## 它能做什么

- **涂哪儿修哪儿**：修妆容、换头发、去瑕疵、改衣服细节、删掉穿帮的路人，其余画面一动不动

- **两条出图路线**：本机 ComfyUI（免费、离线、权重自选），或云端图像接口（不用显卡，按张计费）

- **批量**：一次提交一整个项目里的多张图，按你设的并发一张张排队跑；提交后可以切去别的项目接着涂，
  跑的过程中还能「中断这一张」。关掉窗口就是退出程序，没跑完的下次启动自动重新排队
  
- **原图分辨率出图**：界面里为了流畅只铺缩放档，重绘和缝合都在原始分辨率上做——24MP 的照片
  不会因为你涂得顺手就掉清晰度
  
- **成图接着改**：任意一张结果都能「另存为新图」再涂一轮，或「用这组参数再改一次」把参数搬回右面板

- **人脸感知与一键塑形**：内置 YuNet 人脸检测与 FaceMesh 468 点关键点，使用 tract 在 CPU 上本地推理。
  一键自然塑形，或分别调整瘦脸 / 大眼 / 瘦鼻 / 下巴；提供六种人像构图建议和自动脸部美颜区，无需显卡或生成后端

- **多人局部精修**：选择人脸，分别保存美颜、脸型、眼鼻唇塑形、淡黑眼圈 / 淡眼袋阴影、牙齿美白、眼白提亮和肤色微调；
  轻妆提供腮红、口红、眉毛加深与唇部润泽，口红与腮红可选颜色。
  可保护眉眼、嘴唇、发际线及脸部轮廓外的背景。身体塑形提供瘦腰、肩宽、手臂、腿型和长腿，需手动校准人体框与肩腰胯膝位置。
  全部本地计算；
  
- **不连后端也能出活**（0.3 起）：裁切重构（点一下宽高比就把框摆出来，拖完按「✔ 裁这一版」才生效；
  90° 步进无损、±15° 微调带扩边、翻转）、手动液化（推挤/收缩/膨胀/恢复四把笔刷）、
  基础调色、八色 HSL、RGB / 单通道 / HSL 曲线、三段色彩分级、独立降噪、手动镜头校正、颗粒与暗角，
  内置预设 + 可直接导入的 `.cube` LUT（三线性 / 四面体插值）、独立局部调整蒙版，
  美颜一组七根滑杆：磨皮 / 质感保留 / 祛瑕疵 / 匀肤 / 美白 / 去油光 / 锐化（能只作用在你涂过遮罩的那一块）——
  像素全在你这台机器上算，不叫 ComfyUI、不经云端
  
- **调整完也能 1:1 看**：拖滑杆时铺的是缩放档的快预览，放大到实际像素会自动换成按原始分辨率
  渲染的那一版；不满意随时「清空全部调整」退回源图
  
- **调整是一组参数，不是覆盖**：原图永不改写；预览按缩放档算，落成图才走原始分辨率，
  随时能退回，也能把调整结果「应用为新图」接着涂再交给重绘
  
- **蒙版随时改**：以 PNG 原地存在项目目录里，橡皮可擦回，最多回退 8 笔

- **对比**：原图与成图中缝可拖、可缩放平移，1:1 看真实像素，满意再「下载成图」

- **提示词预设**：常用的修图指令存成模板反复用，负面词与步数/CFG 都能覆盖默认值

- **画布（图生图）**：不依赖照片也能开工——从白纸起稿，写一句话整幅交给云端生成；
  草稿自动落盘，成图挂在同一项目的历史里
  
  

---

## 与图片整理软件双向交接（0.4.1）

同机桌面软件可通过 `/api/interop/v1` 发送项目名、图片 ID、本地交接副本路径及 SHA-256。Synco Photo 导入后可自动打开；正在编辑时，新推送会留在待编辑列表。点击「完成并返回」导出当前精修图；在成图对比中可返回所选 AI 成图。整理软件离线时，结果会一直保留到它领取并确认收到。

- 安装版启动路径：`HKCU\Software\Synco\Interop` 的 `Executable`；便携版需在发送方手动指定一次程序路径。
- 实际端口与实例 ID：`%LOCALAPPDATA%\Synco\interop.json`，通过 `GET /api/interop/v1/info` 校验；不要假定端口固定。隔离运行时可用 `SYNCO_INTEROP_DISCOVERY` 指定独立发现文件。
- 创建：`POST /api/interop/v1/tasks`；状态：`GET /api/interop/v1/tasks/{task_id}?client_id=...`。
- 待领取：`GET /api/interop/v1/tasks?client_id=...&delivery=pending&limit=50`；接收方先复制、校验并登记新版本，再 `POST /api/interop/v1/tasks/{task_id}/ack`。
- 每张图片一个 UUID 任务，相同任务重复发送不会重复导入；来源项目 ID 用于复用项目，图片 ID 用于关联原片。返回文件与原片分开保存，未确认任务阻止误删关联图片。
- 输入 JPEG / PNG / WebP / BMP，单文件最多 256 MiB、6000 万像素；输出 JPEG / PNG。这里不包含 RAW 显影，也不直接覆盖外部软件的原片。

开发者可在构建服务端后运行 `node tools/interop-check.cjs`，模拟发送、接收、重复请求与中断恢复；加 `--browser` 验证自动打开和完成返回的界面。

## 下载与安装

从 [Releases](https://github.com/ShanGuoP/Synco/releases) 下载 **`Synco Photo_x.x.x_x64-setup.exe`**，双击安装
（装到当前用户目录，不需要管理员权限；安装向导只有简体中文）。装完从开始菜单启动 **Synco Photo**。
界面默认中文，切英文在设置 → 外观 → 界面语言——那是程序内部的语言，跟安装向导不是一回事。

> Releases 里还没有东西时，见下面的[从源码构建](#从源码构建)。
> 系统缺 WebView2 运行时的话，安装程序会自己联网补上。

---

## 先准备好"出图"的那一头

Synco Photo 不附带图像生成权重，重绘由本机 ComfyUI 或云端图像接口完成。下面二选一，两个都配好也行，提交时任选。
裁切、调色、液化、美颜和内置人脸分析均可独立离线使用；YuNet 与 FaceMesh 的小模型已随程序打包。

### 路线 A：本机 ComfyUI（推荐，免费、离线）

| 需要 | 说明 |
|---|---|
| ComfyUI | 一个能跑起来的本机实例，Synco Photo 默认找 `http://127.0.0.1:8188` |
| Qwen-Image 2.1 权重 | `qwen_image_2.1_bf16.safetensors`（unet，约 14.2 GB）<br>`qwen3vl_8b_int8_convrot.safetensors`（clip，约 9.4 GB）<br>`qwen_image_2.1_vae_bf16.safetensors`（vae，约 0.68 GB） |
| 一个局部重绘工作流 | 在 ComfyUI 里用 **Save (API Format)** 导出的那份 JSON——**提交用的计算图就是它**。Synco Photo 只往里填照片、遮罩、提示词、种子/步数/CFG 和 LoRA 开关，裁切参数、模型、连线一概照你文件里那样跑 |

## 依赖与素材来源

### ComfyUI

- 仓库：<https://github.com/Comfy-Org/ComfyUI.git>

### Qwen-Image 2.1 权重

国内（hf-mirror 直链）：

- unet：<https://hf-mirror.com/Comfy-Org/Qwen-Image-2.1/resolve/main/diffusion_models/qwen_image_2.1_bf16.safetensors>
- clip：<https://hf-mirror.com/Comfy-Org/Qwen-Image-2.1/resolve/main/text_encoders/qwen3vl_8b_int8_convrot.safetensors>
- vae：<https://hf-mirror.com/Comfy-Org/Qwen-Image-2.1/resolve/main/vae/qwen_image_2.1_vae_bf16.safetensors>

海外（Hugging Face 直链）：

- unet：<https://huggingface.co/Comfy-Org/Qwen-Image-2.1/resolve/main/diffusion_models/qwen_image_2.1_bf16.safetensors>
- clip：<https://huggingface.co/Comfy-Org/Qwen-Image-2.1/resolve/main/text_encoders/qwen3vl_8b_int8_convrot.safetensors>
- vae：<https://huggingface.co/Comfy-Org/Qwen-Image-2.1/resolve/main/vae/qwen_image_2.1_vae_bf16.safetensors>

### 局部重绘工作流

- B 站 [**黑鹤001**](https://space.bilibili.com/515231056/?spm_id_from=333.788.upinfo.detail.click)：<https://www.bilibili.com/video/BV1jmau6jexh/>

### COS LoRA

- B 站 [**AI-Matt**](https://space.bilibili.com/515050794/?spm_id_from=333.788.upinfo.detail.click)：<https://civitai.com/models/2975411/cosv2cos-auto-retouching-v2>

  

三件权重加起来约 24 GB 磁盘，放在 ComfyUI 的 `models/` 下、按类别分子目录即可。

设置 → **工作流参数**：填那份 API 导出，点「角色映射」能看见 Synco Photo 认出的每个节点（谁装图、谁采样、
成图从哪个节点读回来），认不出来的在那里指一次，按文件各存一份。图里必须有「裁切 + 缝合」那一对节点，
缺了会**直接拒绝提交**而不是改用别的图——否则回来的是一张整图重绘，画面看着却正常。
填 UI 导出（save 出来的那份）只读参数，提交用程序内置的计算图，界面与历史记录都会写明用的是哪张。

设置 → **ComfyUI**：填本机 ComfyUI 根目录，点「自检」。它会报认到几个节点包、权重指纹是否对得上、
还缺多少 GB。缺的东西只列路径与体积，**不动你已有的文件**。

再点「生成脚本」，去双击 `data\setup\install_comfyui.bat`：本机没有 ComfyUI 便携包时它会**自己下载并解压**
（约 4 GB，带断点续传与进度），然后克隆缺的节点包、装依赖、逐个核对权重指纹。权重它不替你下——
那份清单里没有可信下载源，得你自己放。

### 路线 B：云端图像接口（不用显卡）

任何 OpenAI 兼容、支持 `images/edits`（收 `image` + `mask` 两个 multipart 字段）的服务都行。

设置 → **云端生成**：填 base_url（接口方给的前缀，含 `/v1`）、模型名、默认输出长边、API key，
勾上"改用云端"，点「测试接口」——它会报响应毫秒数和能不能列出模型。提交时还能临时改这一张的长边。

> API key 明文存在本地数据库里，界面上只显示末四位用于确认，日志不打印。
> 走这条路线时，你提交的那张图会经接口方处理——不想外发的照片就走路线 A。

### 系统要求

- Windows 10 / 11，64 位
- WebView2 运行时（Windows 11 自带；Windows 10 一般随 Edge 已有）
- 路线 A 另需一块跑得动 Qwen-Image 2.1 的显卡

---

## 第一次打开

1. **选资料目录** —— 第一次启动会把这一页直接摊开。原图、蒙版、成图和数据库都放这儿，可以指到任意盘；
   选完可能要重启一次。
2. **接上出图的那一头** —— 需要 AI 重绘时，见上面两条路线，任选其一（或都配）；只做本地调整可跳过。
3. **建项目、导入照片** —— 首页那张「照片导入」卡：把照片拖上去，或点「选择」多选。
   项目名留空就按日期自动起一个。
4. **涂遮罩** —— 打开图片，用画笔在要修的地方涂；白色那块就是会被重绘的区域，橡皮擦回、`Ctrl + Z` 撤销。
   滚轮以光标为锚点缩放，`0` 适应窗口，`1` 看实际像素。
5. **写指令、提交** —— 右侧面板写清正面的指令与负面提示词，本机路线还能调步数 / CFG / 种子
   （云端模式没有这几样，负面词会并进正向一起发出），提交后进度会走完「排队 → 采样 → 缝合」三段。
   批量提交时每张在种子基础上叠加图片序号，同批不重复、又彼此可比。完成后拖中缝对比原图与成图。

---

## 日常使用

首页是项目列表（拼贴封面 + 已涂张数），左栏是主页 / 全部项目 / 待涂遮罩 / 已涂待提交与最近打开，
项目页里可以「导入更多」、勾选多张批量提交、翻每张图片的历史结果。

顶栏那枚徽章写着当前的生成方式与参数，旁边是「刷新」「设置」（后端 / 工作流 / ComfyUI 目录）和「帮助」。
换成本机 ComfyUI 还是云端，在设置 → 云端生成里勾"改用云端"。

**设置**分十块：ComfyUI、工作流参数、云端生成、图像档位、外观、导出目录、提示词短语、参数预设、数据目录、关于。
「外观」里管三样：主题底色、动效强弱、界面语言。「图像档位」是编辑时铺的那一档长边（默认 3072）——调小更流畅，调大涂得更细。

**快捷键**

| 键 | 作用 |
|---|---|
| `B` / `E` / `H` | 画笔 / 橡皮 / 抓手平移 |
| `空格`（按住） | 临时平移 |
| `Ctrl + Z` | 撤销一笔 |
| `滚轮` | 以光标为锚点缩放 |
| `0` / `1` | 适应窗口 / 实际像素 1:1 |
| `Ctrl + Enter` | 提交生成 |
| `Esc` | 退出对比 / 关闭弹窗 |
| `?` | 快捷键表 |

**本地调整**（右栏「本地调整」那一页）：按基础、色彩、曲线、HSL、色彩分级、细节、镜头校正、效果、局部调整、美颜分区。
基础与色彩默认展开，每个分区可单独重置。美颜、人脸精修、身体塑形和手动液化统一放在最底部「美颜」里；裁切旋转位于「镜头校正」。原图始终保留。
曝光以 EV 显示，自动优化按照片亮度分布给出温和起点；白平衡吸管用中性灰区域取样。
局部调整支持独立图层、径向 / 线性渐变和画笔、羽化、反选及擦除；进入蒙版编辑时显示原图坐标，完成后回到整条处理链，蒙版随裁切、旋转和塑形映射。
可表示为颜色查表的调整使用 WebGL2 交互预览（标记为近似）；停下后以 Rust CPU 确认图为准。降噪、液化等空间处理使用 CPU；导出始终用原件和保存的参数重算。
裁切的入口就是那一排宽高比——点一颗，画面上就摆出框来（「自由」= 摆框不锁比例，再点当前那颗收起），
拖手柄或整框调位置，**只有按「✔ 裁这一版」才真的落定**，不点就等于没裁。旋转/翻转是即时的，
90° 那一档不重采样。液化先选笔刷再「开始画形」，一笔一笔记在参数里，「退一笔」「清掉笔画」随时反悔。
调完想接着涂遮罩交给重绘：先「应用为新图」，它会带着这套参数变成项目里的另一张图，源图那份参数还在。

### 人脸感知与一键塑形

打开照片时会自动分析人脸，已有结果直接复用；首次加载模型可能稍慢。照片与关键点推理均在本机处理。
在右栏切到「本地调整」，展开最底部「美颜」中的「人脸感知与一键塑形」：

1. **塑形**：点击「一键自然塑形」应用温和参数，或单独调整瘦脸、大眼、瘦鼻、下巴。
   四项量程均为 `-100～100`，`0` 保留原貌；「重置塑形」仅清掉这四项，手动液化笔画仍保留。
   多人照片按面积选择最大的主脸进行塑形。
2. **构图**：选择 `1:1`、`4:3`、`3:4`、`16:9`、`3:2` 或 `9:16`，先在画面上预览建议框，
   仍可拖动调整；按「✔ 裁这一版」才生效。建议框会适配已有的旋转与翻转。
3. **美颜区**：在「美颜」中开启「自动人脸美颜区」，再调整磨皮等滑杆。
   美颜区覆盖所有关键点可用的人脸皮肤，避开眼睛和嘴唇；同时开启涂抹蒙版限制时，取两者交集。
   该开关默认关闭，开启本身不会自动设置美颜强度。

没有检出人脸或主脸关键点不可用时，自动塑形按钮与滑杆会禁用，手动液化、裁切和调色仍可使用。
「重新检测人脸」可重新分析；无可用人脸时自动美颜区为空，不会退回全图处理。
原图始终不被改写，塑形与美颜区会随裁切、旋转和翻转映射到调整后的画面。
模型来源、SHA-256 与许可证见 [模型说明](assets/face/README.md) 和 [NOTICE](NOTICE.md)。

**导出**：设置 → 导出目录填一个本机文件夹（不存在会自动建），然后在精修页顶栏点「导出」，
这张当前的成图会以 `<原图名_#结果号>.png` 交出去（重名自动加 (2)(3)）；对比视图里另有
「下载成图 / 裁切图 / 遮罩叠加」三个直链，第一条同样走无损。
交出去的那一张是**按这一行提交时的参数重算**出来的：盘上存着的那档成图是 q95——24MP 一张无损
PNG ≈ 21 MB，历史堆不起。无损原件由三样拼回来：模型回来的那张窗口图（原样字节，实测 1–3 MB 量级）、
这一枪当时用的那份遮罩、当时那套缝合与调整参数。哪一样不在了（最常见是原图被手动删过），
导出的文件名就留下 `.jpg` 并在提示里说清为什么给不出无损——不会默默把次档当原件交给你。

---

## 资料放在哪儿

默认贴着程序走：`<程序目录>\data\`。把整个文件夹拷到 U 盘或另一台机器，资料就跟着走（便携模式）。
装在 `Program Files` 这种没写权限的地方时，会自动改存 `%LOCALAPPDATA%\Synco\data`。
设置 → 数据目录可以指到任意盘，也能「把资料复制到新目录…」。

里面 `app.db` 是数据库，`projects/<项目号>/` 是原图、蒙版、成图和各级缩略档，`runtime/` 是锁、日志
（进程私有的东西全收在这，不摊在根目录上），`runtime/cache/` 是**可再生档**——放大要看的那些瓦片、
「本地调整」的预览与成图，全在这一层，删掉不丢任何一个像素，下一次看那张图会现切（24MP 约半秒）。
设置 → 图像档位可以给这一层设一个上限（默认 4 GB，填 0 = 不限），超了就丢最久没重切的那一张；
也可以一键清掉。**要备份就得整个目录一起拿**，只有 `runtime/` 可以不带。
老版本把这些档放在 `projects/<项目号>/tiles/` 和原图旁边，第一次启动会认出那一族的旧文件名并收掉它们
（只收瓦片与预览/成图，被库里的记录指着的那些一张不动）。
成图那一档存的是 q95（`r#_…_final.jpg`），旁边跟着两份小档：`_raw.png`（模型回来的窗口图）与
`_msnap.png`（那一枪当时用的遮罩）。它们是"随时能把无损那一张重算出来"的原料，不是又一份成图；
删记录、删图、删项目都会一起清掉，不留没人认领的文件。

**别同步到网盘。** 库里存着明文的云端 API key，而且网盘的按需拉取/占位文件会让 SQLite 的 WAL 写坏。
换电脑就用设置里的「把资料复制到新目录」（它会先把 WAL 合并干净再整棵复制，目标目录已有库时会拒绝覆盖），
或者直接把整个程序文件夹拷过去。

---

## 常见问题

**支持哪些图片格式？**
`jpg` / `png` / `webp` / `bmp`。手机直出的 HEIC、相机 RAW 和 avif 请先转成 jpg 再导入。
拖进来的非图片会被跳过并告诉你跳过了几个。

**端口 7861 被占了怎么办？**
不用管，启动时自动换一个随机端口，窗口打开的就是正确地址。程序在后台跑一个**只监听本机**的服务，
窗口是它的一层壳。想固定端口就设环境变量 `SYNCO_PORT`。

**关掉窗口，正在跑的任务会丢吗？**
关窗会退出整个程序，队列跟着停，但没跑完的那些不丢——下次启动自动重新排队。
**唯一接不回来的是已经在飞的那一次云端请求**：那张会停在中断状态，重新提交即可，
注意这次被丢掉的请求可能已经计过费了。要出门就让程序开着别关。

**双击两次会开出两个窗口吗？**
不会，第二次会把已经在跑的那个叫到前面。这是刻意的：两个进程共写同一个 `app.db` 会把库写坏。

**遮罩涂到画面边上，会不会贴回时露出原图？**
不会。贴边的羽化权重按复制边缘算，最后一行/一列仍是满权重替换。（早期版本在这里会露一条 100px 上下的边带。）

**ComfyUI 报「少了输出：<节点号>」**
提交用的图来自你的工作流时，先核对设置 → 工作流参数 → 「角色映射」里那三路输出指的节点：
被指到的节点没把图写进 history（比如它被禁用了、或压根不是出图节点）就会报这一句。
指错了直接改回来；那张图里本来没有裁切预览/遮罩预览，就把这两个可选角色留空，只留「输出成图」。

**蒙版涂得很细，出图会糊吗？**
不会。你涂的是缩放档，落盘的蒙版会被还原到原图分辨率再做重绘与缝合。

**导出的那张为什么变成 `.jpg` 了？**
那一行的无损重算前提缺了一件：原图被手动删过、窗口原图或当时那份遮罩的快照丢了、
或者缝合的规则在这之后改过版本（改过之后重算出来的已经不是当年那一张，宁可不给）。
提示里会写明是哪一种。盘上那档成图本来就是 q95 预览，所以**要交付就尽早导**——
窗口原图与遮罩快照在，无损就随时拿得回来。一张 24MP 的重算实测约 0.5 秒（发布档；`cloud-e2e` 会把这个数打出来），
所以"要无损就导出"不是等半天。

**一次能拖多少张？**
没有硬性上限，导入会自己分批上传。单张原图超过几十 MB 时建议先压一下。

**切了英文，为什么历史记录里那条报错还是中文？**
因为它落库时就是那句中文——老记录不回填、不猜。新跑出来的失败会把钥匙与参数一起存着，
按你当前的语言现查现拼。上游（ComfyUI / 云端 / 操作系统）带回来的原文两边都不翻。

---

## 从源码构建

需要 Rust 1.99 或更新（`Cargo.toml` 里声明了 `rust-version`）。

```bash
git clone https://github.com/ShanGuoP/Synco.git
cd Synco
cargo build --release        # 得到 synco.exe / synco-tools.exe / synco-desktop.exe
powershell -NoProfile -ExecutionPolicy Bypass -File tools/build.ps1
# 得到 NSIS 安装包 target/release/bundle/nsis/Synco Photo_<版本号>_x64-setup.exe（版本号读自 Cargo.toml）
```

打包前会检查 Tauri 权限缓存中的绝对路径。移动项目或复制旧 `target` 后，如果路径失效，会只清理相关 Tauri crate 的构建缓存并重新生成，不删除照片资料或已有安装包。直接运行 Tauri CLI 的 `build` 也会执行同一检查。

只想跑起来看看，不用打包：

```bash
target\release\synco.exe     # 起本机服务，然后浏览器打开 http://127.0.0.1:7861
```

前端是原生 JS + 手写 CSS，**没有构建步骤、没有 npm 依赖**，改完刷新就见效。

跑一遍自检确认环境没问题：

```bash
cargo test --workspace       # 缝合内核与服务的回归
node tools/api-check.js      # 接口扫查 + 路径穿越回归，自己起隔离实例，不碰你的数据
node tools/cloud-e2e.js      # 云端链路端到端，用假云端，不消耗你的额度
node tools/comfy-e2e.js      # 本机那条：用假 ComfyUI 验工作流接管与角色映射
node tools/lint-layers.js    # 分层纪律八条，含"已交钥匙的文件不许再写中文文案"
node tools/i18n-check.js     # 中英键位对齐、还剩多少界面文案没进语言包
```

验证本地调整与人脸链路时，先构建测试脚本默认使用的开发版服务。人脸测试需自备一张清晰的 JPEG / PNG 人像，
测试在独立临时数据目录运行，不读取或改动你的项目资料：

```powershell
cargo build -p synco-server --bin synco
node tools/adjust-check.js
$env:SYNCO_FACE_TEST_IMAGE = 'C:\Photos\portrait.jpg' # 改成你自己的测试人像路径
cargo test -p synco-server real_portrait --lib -- --ignored
node tools/face-check.cjs
```

人脸检查包含真实单人 / 双人检测与关键点、塑形像素变化，以及接口层的参数保存、预览复用、成图、
旋转后的构图建议和无脸结果缓存。默认 `cargo test` 会跳过需要人像文件的测试。

`synco-tools.exe` 只在源码构建时产出（安装包里没有它）。不带参数运行是一个带实时状态的交互菜单：
启停本机服务、补齐派生档、锁定权重指纹、体检 ComfyUI 目录、切换生产等，每一步都要你确认。

---

## 安全说明

- 服务**只监听 `127.0.0.1`**，不对外网开放，并校验请求的 Host 必须是本机地址
- 写操作校验来源，取图接口只放行图片格式并挡住目录穿越
- 云端 API key 只存在本地数据库，界面只显示末四位，日志不打印
- 你的照片不会上传到任何第三方——除非你自己选了云端路线并提交了那张图

---

## 许可

Apache License 2.0，全文见 [LICENSE](LICENSE)。

```
Copyright 2026 ShanGuoP
```

### 第三方资源

界面字体 **思源宋体 / Noto Serif SC**（运行时加载按常用字子集化的 `public/fonts/NotoSerifSC-VF.woff2`，
完整字源 TTF 已退役出工作区、git 历史留档）
单独适用 **SIL Open Font License 1.1**，不受上面的 Apache-2.0 覆盖：

```
Copyright (c) 2017-2024 Adobe (http://www.adobe.com/).
Noto is a trademark of Google Inc.
```

授权全文见 [`public/fonts/OFL.txt`](public/fonts/OFL.txt)。把 24 MB 的字体打进仓库是为了离线可用——
界面不依赖任何 CDN。

---

## 致谢

感谢以下开源项目为 Synco Photo 提供的程序依赖、模型与算法参考。各条目说明实际用途；
版本号、许可证与算法参考范围见 [NOTICE](NOTICE.md)。

**桌面壳与界面**

- [Tauri 2](https://tauri.app)（含 single-instance / dialog 插件）—— 把服务、窗口和 WebView2 收进一个 exe，
  以及"双击第二次是把已经开着的窗口叫到前面"
- [rust-embed](https://github.com/pyros2097/rust-embed) —— 整个前端打进 exe，装到哪儿都能离线打开
- **思源宋体 / Noto Serif SC**（SIL OFL 1.1）—— 界面与标题的中文衬线，上游与授权见 [`public/fonts/OFL.txt`](public/fonts/OFL.txt)

**服务与数据**

- [axum](https://github.com/tokio-rs/axum) + [tokio](https://github.com/tokio-rs/tokio) —— 只监听 127.0.0.1 的本机接口，
  以及"重活一律挪出异步 worker"那条纪律靠的阻塞线程池
- [rusqlite](https://github.com/rusqlite/rusqlite)（内含 public domain 的 SQLite）—— 项目、提交历史与参数链都存在一个文件里
- [serde](https://github.com/dtolnay/serde) / [serde_json](https://github.com/dtolnay/serde_json)、
  [thiserror](https://github.com/dtolnay/thiserror)、[url](https://github.com/servo/rust-url)、
  [pathdiff](https://github.com/manisheus/pathdiff)、[futures-util](https://github.com/rust-lang/futures-rs) —— 参数、错误与路径的底座

**图像处理**

- [image](https://github.com/image-rs/image) + [zune-jpeg](https://github.com/etemesi254/zune-image) —— 解码，
  以及把 EXIF 方向烘焙进像素（不然"原图正着看、缩略图横着躺"）
- [fast_image_resize](https://github.com/Cykooz/fast_image_resize) —— 缩放档、缩略图与调整预览的重采样
- [imageproc](https://github.com/image-rs/imageproc) —— 蒙版膨胀（缝合前把笔迹外扩那一圈）
- [moving-least-squares](https://github.com/mpizenberg/rust_mls)（MPL-2.0）—— 一键塑形使用的 MLS 形变数学。
  它以原样 crate 依赖，没有被改动、也没有把源码复制进本仓库，所以只要求它自己那几个文件继续留在 MPL-2.0 下

**人脸感知**

- [tract](https://github.com/sonos/tract) —— YuNet 与 FaceMesh 的本地 CPU 推理
- [OpenCV Zoo YuNet](https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet) —— 人脸框与五点检测
- [MediaPipe FaceMesh ONNX](https://github.com/yakhyo/mediapipe-face-mesh-onnx) —— 468 点关键点；模型与拓扑来自 Google MediaPipe，
  由 Yakhyo 导出，权重来源经 PINTO0309 的转换。模型许可与固定校验值见 [模型说明](assets/face/README.md)

**人像塑形算法参考**

- [GPUPixel](https://github.com/pixpark/gpupixel) —— 参考 `FaceReshapeFilter` 的局部径向大眼、作用半径与距离衰减思路，
  在 Rust 中实现眼睛内部和周边纹理的连续反向映射。[参考源码](https://github.com/pixpark/gpupixel/blob/ef552bf8ce2d0d41fa9b979bfb5c7cf79374ca88/src/filter/face_reshape_filter.cc)
- [FaceSlim](https://github.com/SysAdminDoc/FaceSlim) —— 参考按五官组织控制点，以及向脸部轮廓外扩展、羽化背景保护区域的设计；
  塑形保护与皮肤美颜使用不同蒙版，让瘦脸可以移动轮廓。[参考源码](https://github.com/SysAdminDoc/FaceSlim/blob/6521f8d118d3c7062d4dfa2279e0160e05ccc8f5/faceslim/pipeline.py)
- [CainCamera](https://github.com/CainKernel/CainCamera) —— 参考瘦脸、削脸、下巴、大眼各自独立的控制尺度，以及像素坐标中的五官距离；
  Synco Photo 分别限制各五官的叠加位移。[参考源码](https://github.com/CainKernel/CainCamera/blob/8d1270e2a4d1e0e69940cf806c5c4ae39615eb66/filterlibrary/src/main/assets/shader/face/fragment_face_reshape.glsl)

这些项目用于算法设计参考，未作为 SDK 或运行时依赖打包。Synco Photo 的实现位于
`crates/photoedit-core/src/warp/face.rs` 与 `crates/photoedit-core/src/portrait.rs`，由 Rust 完成预览与成图计算。

**收发与杂项**

- [reqwest](https://github.com/seanmonstar/reqwest)（含 rustls / ring）—— 调本机 ComfyUI 与云端图像接口，带进度与中断
- [base64](https://github.com/marshallpierce/rust-base64)、[sha2](https://github.com/RustCrypto/hashes)、
  [rand](https://github.com/rust-random/rand) —— 图片进出、资源指纹、随机种子
