# Synco 新刻

**[简体中文](README.md)** · English

**Only what you painted gets repainted. Every other pixel stays put.**

Import a photo → paint over the part you want fixed → hand it to your local ComfyUI or a cloud image API → get it stitched back into the original.

Unlike regenerating the whole frame — which quietly changes your background, the fabric texture on the shirt and the face you didn't ask about — Synco replaces pixels inside the mask and nothing else. Outside the feathered transition band the result is **bit-identical** to the original. The seam is closed with multi-band blending, so there's no pasted-on border.

This demo started tiny: a Node tool with one button in the rail — **GPT Image redraw**. Then it needed ComfyUI, and the original image handling hit a performance wall (scrubbing through photos kept freezing) that the old code couldn't grow past, and I couldn't fix it from where I was — CRUD is about as deep as I go. So I rewrote it. I went back and forth on the design with 大肥鱼 DeepSeek-v4.1-Flash, GLM-5.3-Flash, GPT-6.1-Sol and Qwen-3.8-Flash over a few rounds and landed on a Rust rewrite. Go was the plan — I know it better — but arguing with the models made it clear Rust is the stronger fit for image work. Fine. Rust it is.

Most of the Rust code was written by Qwen-3.8-Flash, then cross-audited by GLM-5.3, DeepSeek-v4.1-Flash and GPT-6.1-Sol.

---

## What it does

- **Fix what you painted**: makeup, hair, blemishes, clothing detail, deleting a passer-by in the frame — the rest of the image doesn't move
- **Two ways to get pixels**: local ComfyUI (free, offline, your own weights) or a cloud image API (no GPU, billed per image)
- **Batch**: submit many images from one project at once; they queue and run at the concurrency you set. You can switch to another project and keep painting while they run, and interrupt an individual image. Closing the window quits the app; whatever hasn't finished is re-queued on next launch
- **Output at original resolution**: the viewer shows a scaled-down level for smoothness, but repainting and stitching happen at full resolution — a 24MP photo doesn't lose detail because painting was easier on a smaller canvas
- **Keep editing the result**: any result can be "Save as new image" and masked again, or "Run again with these settings" to pull them back into the right-hand panel
- **Useful without a backend** (since 0.3): crop & straighten (tap an aspect ratio and the frame appears; drag it, then "✔ Crop this" to commit — 90° steps are lossless, ±15° fine rotation gets expanded edges, plus flips), manual liquify (four brushes: push / pinch / enlarge / restore), ten colour sliders + built-in presets + `.cube` LUTs (drop a `.cube` file into `luts/` in your data folder and it shows up in the dropdown), plus a Beauty group of seven sliders — smoothing, texture retention, blemishes, even tone, brightening, de-shine, sharpening (can be limited to the area you painted) — all computed on your machine, no ComfyUI call, nothing uploaded
- **See it at 1:1 after adjusting**: while you drag a slider you get a fast scaled preview; zoom to actual pixels and it swaps to the full-resolution render. "Clear all adjustments" takes you back to the source at any time
- **Adjustments are parameters, not an overwrite**: the original is never rewritten; previews run on the scaled level and only the final render uses full resolution. You can always go back, and you can apply the result as a new image and hand *that* to the repaint path
- **Masks stay editable**: stored in the project folder as a PNG that gets overwritten in place, erasable, up to 8 undo steps
- **Compare**: draggable divider between original and result, zoom and pan, 1:1 on real pixels, then "Download result"
- **Prompt presets**: save the instructions you reuse; negatives and steps/CFG can override the defaults
- **Canvas (image-to-image)**: start from a blank sheet — write one line and generate the whole frame in the cloud; drafts auto-save and results hang off the same project's history
- **Drag to import**: many files at once, or drop a whole folder onto the import card on the home page
- **Light and dark**: follow system / paper white / ink black; the area around the canvas always stays a neutral dark, so judging colour isn't thrown off by the UI background
- **Bilingual interface**: Settings → Appearance → Language, one click. Every sentence lives in a language pack, so errors and queue status follow the language you picked. What doesn't get translated: prompts sent to the model, names you typed, and the raw text your OS or backend returned — translating those would be editing your data
- **Restrained motion**: page turns, theme switches and tile entrances animate; the canvas area never does (no flashing where colour is judged). There's a "reduce motion" switch in Settings with the same meaning as `prefers-reduced-motion`

---

## Download and install

Grab **`Synco_x.x.x_x64-setup.exe`** from [Releases](https://github.com/ShanGuoP/Synco/releases) and double-click it.
It installs into your user profile, needs no admin rights, and the installer wizard itself is Simplified-Chinese only.
Launch **Synco** from the Start menu afterwards. The interface starts in Chinese; English is a switch inside the app —
Settings → Appearance → Language. That's the program's language, not the installer's.

> If Releases is still empty, see [Building from source](#building-from-source) below.
> If the WebView2 runtime is missing, the installer pulls it from the internet.

---

## Get an image engine ready first

Synco ships no model. It's a **dispatch desk plus stitching core**. Pick one of the two below — or configure both and choose per submit.

### Route A: local ComfyUI (recommended — free, offline)

| What you need | Notes |
|---|---|
| ComfyUI | A local instance that actually runs; Synco looks at `http://127.0.0.1:8188` by default |
| Qwen-Image 2.1 weights | `qwen_image_2.1_bf16.safetensors` (unet, ~14.2 GB)<br>`qwen3vl_8b_int8_convrot.safetensors` (clip, ~9.4 GB)<br>`qwen_image_2.1_vae_bf16.safetensors` (vae, ~0.68 GB) |
| One inpainting workflow | The JSON you exported from ComfyUI with **Save (API Format)** — that file *is* the computation graph we submit. Synco only fills in the photo, mask, prompt, seed/steps/CFG and LoRA switches; crop parameters, models and wiring run exactly as your file has them |

## Dependencies and where the material comes from

### ComfyUI

- Repository: <https://github.com/Comfy-Org/ComfyUI.git>

### Qwen-Image 2.1 weights

Via hf-mirror (fast in mainland China):

- unet: <https://hf-mirror.com/Comfy-Org/Qwen-Image-2.1/resolve/main/diffusion_models/qwen_image_2.1_bf16.safetensors>
- clip: <https://hf-mirror.com/Comfy-Org/Qwen-Image-2.1/resolve/main/text_encoders/qwen3vl_8b_int8_convrot.safetensors>
- vae: <https://hf-mirror.com/Comfy-Org/Qwen-Image-2.1/resolve/main/vae/qwen_image_2.1_vae_bf16.safetensors>

Direct from Hugging Face:

- unet: <https://huggingface.co/Comfy-Org/Qwen-Image-2.1/resolve/main/diffusion_models/qwen_image_2.1_bf16.safetensors>
- clip: <https://huggingface.co/Comfy-Org/Qwen-Image-2.1/resolve/main/text_encoders/qwen3vl_8b_int8_convrot.safetensors>
- vae: <https://huggingface.co/Comfy-Org/Qwen-Image-2.1/resolve/main/vae/qwen_image_2.1_vae_bf16.safetensors>

### The inpainting workflow

- Bilibili [**黑鹤001**](https://space.bilibili.com/515231056/?spm_id_from=333.788.upinfo.detail.click): <https://www.bilibili.com/video/BV1jmau6jexh/>

### COS LoRA

- Bilibili [**AI-Matt**](https://space.bilibili.com/515050794/?spm_id_from=333.788.upinfo.detail.click): <https://civitai.com/models/2975411/cosv2cos-auto-retouching-v2>

  
The three weights are about 24 GB on disk. Put them under ComfyUI's `models/`, sorted into the category subfolders.

Settings → **Workflow parameters**: point it at that API export. "Role mapping" shows every node Synco
recognised (who loads the image, who samples, which node the finished image is read back from); point at the
ones it can't detect, stored per file. The graph must contain the crop + stitch pair of nodes — if they're
missing, submission is **refused outright** rather than silently swapped for a different graph, because
otherwise you'd get a whole-frame repaint back that looks perfectly fine.
A UI export (plain save) only supplies parameter values; submission uses the built-in graph, and both the
panel and the history record say which graph was used.

Settings → **ComfyUI**: set your local ComfyUI root and run the self-check. It reports how many node packs it
recognised, whether weight fingerprints match the baseline, and how many GB are still missing. Missing items are
listed with paths and sizes only — **it doesn't touch your files**.

Then hit "Generate script" and double-click `data\setup\install_comfyui.bat`: when there's no ComfyUI portable
pack on the machine it downloads and unpacks one (~4 GB, resumable, with progress), then clones the missing node
packs, installs dependencies and checks every weight fingerprint. It won't download the weights for you — that
manifest has no trustworthy sources in it, so you place them yourself.

### Route B: cloud image API (no GPU needed)

Anything OpenAI-compatible that supports `images/edits` (accepting the `image` and `mask` multipart fields) works.

Settings → **Cloud generation**: fill in base_url (the prefix your provider gave you, `/v1` included), model name,
default output long edge and API key, tick "use the cloud path", then press "Test endpoint" — it reports response
milliseconds and whether it can list models. You can also change that one image's long edge at submit time.

> The API key is stored in plaintext in the local database. The UI shows the last four characters for confirmation
> and the log never prints it.
> On this route, the image you submit is processed by the provider — keep photos you'd rather not send on route A.

### System requirements

- Windows 10 / 11, 64-bit
- WebView2 runtime (built into Windows 11; on Windows 10 it's usually already there with Edge)
- Route A additionally needs a GPU that can run Qwen-Image 2.1

---

## First launch

1. **Choose your data folder** — on the very first start this page opens by itself. Originals, masks, results and
   the database all live here; point it at any drive. You may need one restart afterwards.
2. **Connect an image engine** — see the two routes above; either one is enough (or configure both).
3. **Create a project, import photos** — the "Photo import" card on the home page: drop files on it, or press
   "Import photos" and multi-select. Leave the project name empty and it names itself after the date.
4. **Paint the mask** — open an image and paint over what should be fixed; the white area is what gets repainted.
   Erase to take it back, `Ctrl + Z` to undo a stroke. The wheel zooms around the cursor, `0` fits the window,
   `1` shows actual pixels.
5. **Write the instruction, submit** — the right panel holds the positive prompt and the negative one; on the local
   route you also get steps / CFG / seed (the cloud path has none of those, and the negative is merged into the
   positive before sending). Progress walks through queued → sampling → stitched. In a batch, each image adds its own
   index to the seed, so nothing repeats inside a batch and the results stay comparable. When it's done, drag the
   divider to compare original and result.

---

## Day to day

The home page is the project list (collage cover + how many are painted). The left rail is Home / All projects /
Needs a mask / Masked, ready to run / Canvas, plus recently opened. Inside a project you can "Import more",
tick several images and submit them together, and page through each image's history.

The top bar's badge shows the current generation route and its parameters, next to "Refresh", "Settings"
(backend / workflow / ComfyUI folder) and "Help". Switching between local ComfyUI and the cloud is the
"use the cloud path" tick under Settings → Cloud generation.

**Settings** has ten panes: ComfyUI, Workflow parameters, Cloud generation, Image level, Appearance, Export folder,
Prompt phrases, Parameter presets, Data folder, About. Appearance owns three things: theme colour, motion strength,
interface language. "Image level" is the long edge of the level laid down while editing (3072 by default) —
smaller is smoother, larger lets you paint finer.

**Shortcuts**

| Key | What it does |
|---|---|
| `B` / `E` / `H` | Brush / Eraser / Hand (pan) |
| `Space` (held) | Temporary pan |
| `Ctrl + Z` | Undo one stroke |
| `Wheel` | Zoom around the cursor |
| `0` / `1` | Fit window / actual pixels 1:1 |
| `Ctrl + Enter` | Submit for generation |
| `Esc` | Leave compare / close the dialog |
| `?` | Shortcut list |

**Local adjustments** (the "Local edit" page in the right rail): four groups of parameters — crop, liquify,
colour, beauty — and the original is never rewritten end to end. The crop entry point is the row of aspect ratios:
tap one and the frame appears on the canvas ("Free" places the frame without locking the ratio; tapping the current
one collapses it). Drag the handles or the whole frame, and **nothing commits until you press "✔ Crop this"** —
skip it and it's as if you never cropped. Rotation and flips apply immediately; the 90° step isn't resampled.
Liquify: pick a brush, then "Start shaping"; every stroke is recorded in the parameters, and "Undo one" /
"clear strokes" let you back out. If you want to keep painting after adjusting: "Apply as new image" turns it into
another image in the project carrying those parameters, while the source keeps its own.

**Export**: set a local folder under Settings → Export folder (it's created if missing), then press "Export" in the
retouch page's top bar — the current result is copied there as `<origname_#result>` (`.png`), with `(2)(3)` appended
on a name clash. The compare view has direct links for "Download result / crop / mask overlay".

---

## Where your stuff lives

By default it travels with the program: `<app folder>\data\`. Copy the whole folder to a USB stick or another
machine and the library comes with it (portable mode). Installed somewhere without write permission —
`Program Files`, say — it falls back to `%LOCALAPPDATA%\Synco\data`.
Settings → Data folder can point anywhere, and there's "Copy the data to a new folder…".

Inside it: `app.db` is the database, `projects/<id>/` holds originals, masks, results and every thumbnail level,
and `runtime/` holds the lock and logs (everything process-private is kept in there, not spread across the root) —
**to back up, take the whole folder**; only `runtime/` can be left out.

**Don't sync it to cloud storage.** The database holds your cloud API key in plaintext, and on-demand fetching /
placeholder files from cloud drives corrupt SQLite's WAL. To move machines, use "Copy the data to a new folder"
(it checkpoints the WAL before copying the tree, and refuses to overwrite a folder that already has a library),
or just copy the whole program folder across.

---

## FAQ

**Which image formats?**
`jpg` / `png` / `webp` / `bmp`. Convert phone HEIC, camera RAW and avif to jpg first. Non-image files dropped in get
skipped, and it tells you how many.

**Port 7861 is taken — now what?**
Nothing to do: it picks a random port at startup and the window opens on the right address. The program runs a local
service bound to **localhost only**; the window is a shell over it. Set `SYNCO_PORT` if you want a fixed port.

**If I close the window, is the running job lost?**
Closing the window quits the app and the queue stops with it, but unfinished work isn't lost — it's re-queued on the
next launch. **The one thing that can't be recovered is the cloud request already in flight**: that image stays in the
interrupted state and you re-submit it. Mind that the dropped request may already have been billed. Going out? Leave
the program running.

**Does double-clicking twice open two windows?**
No — the second launch brings the running window to the front. Deliberate: two processes writing the same `app.db`
corrupts the library.

**I painted to the edge of the frame — will the original show through when it's pasted back?**
No. The feather weights near the border are computed with edge replication, so the last row/column is still a
full-weight replacement. (Early versions leaked a band roughly 100px wide there.)

**ComfyUI says "returned too little: <node id>"**
When the submission graph comes from your workflow, check the three output roles under
Settings → Workflow parameters → Role mapping: the error means the node it points at never wrote an image into
history (disabled, or simply not an output node). Point it somewhere sensible; if your graph genuinely has no crop
preview or mask preview, leave those two optional roles empty and keep only "Save image".

**I mask very finely — does the output come out soft?**
No. You paint on the scaled level, but the mask written to disk is restored to original resolution before repainting
and stitching.

**How many can I drop at once?**
No hard cap; imports upload themselves in batches. For originals in the tens of MB, compress first.

**I switched to English — why is that old error still Chinese?**
Because the sentence stored with that record *was* Chinese when it happened. Old rows are not back-filled and not
guessed. Failures produced from now on store a key plus its arguments and get resolved in whatever language you're
reading the interface in. Raw text from upstream (ComfyUI / the cloud / your OS) is never translated, in either language.

---

## Building from source

Needs Rust 1.99 or newer (`rust-version` is declared in `Cargo.toml`).

```bash
git clone https://github.com/ShanGuoP/Synco.git
cd Synco
cargo build --release        # gives you synco.exe / synco-tools.exe / synco-desktop.exe
npx @tauri-apps/cli build    # gives you the NSIS installer target/release/bundle/nsis/Synco_0.3.1_x64-setup.exe
```

Just want a look, no packaging:

```bash
target\release\synco.exe     # starts the local service, then open http://127.0.0.1:7861 in a browser
```

The frontend is plain JS with hand-written CSS: **no build step, no npm dependencies**. Edit, refresh, done.

Run the self-checks to prove the environment is sane:

```bash
cargo test --workspace       # stitching core and service regressions
node tools/api-check.js      # endpoint sweep + path-traversal regression; spins up its own isolated instance
node tools/cloud-e2e.js      # cloud path end to end against a fake cloud; spends none of your quota
node tools/comfy-e2e.js      # local path against a fake ComfyUI: workflow takeover and role mapping
node tools/lint-layers.js    # eight layering rules, incl. "files that hand copy to the dictionary may not hardcode it"
node tools/i18n-check.js     # zh/en key parity and how much interface text is still untranslated
```

`synco-tools.exe` only exists in a source build (it isn't in the installer). Run with no arguments and it's an
interactive menu with live status: start/stop the local service, backfill derived levels, lock weight fingerprints,
check a ComfyUI folder, switch production — every step asks before it acts.

---

## Security notes

- The service listens on **127.0.0.1 only**, is never exposed to a network, and checks that the request Host is a local address
- Writes check the origin; the image fetch route only serves image formats and blocks path traversal
- The cloud API key lives in the local database only; the UI shows the last four characters, the log prints nothing
- Your photos go nowhere unless you choose the cloud route and submit that image

---

## License

Apache License 2.0 — full text in [LICENSE](LICENSE).

```
Copyright 2026 ShanGuoP
```

### Third-party resources

The interface font is **Noto Serif SC / 思源宋体** (at runtime it loads the subset
`public/fonts/NotoSerifSC-VF.woff2`; the full-source TTF was retired from the workspace and stays in git history).
It is covered by the **SIL Open Font License 1.1** on its own terms, not by the Apache-2.0 above:

```
Copyright (c) 2017-2024 Adobe (http://www.adobe.com/).
Noto is a trademark of Google Inc.
```

The full licence is in [`public/fonts/OFL.txt`](public/fonts/OFL.txt). Committing a 24 MB font is about being offline —
the interface doesn't depend on any CDN.

---

## Thanks

Synco stands on these open-source projects. The ones below are **actually in the shipped build**, grouped by the work
they did; versions and per-item licences are in [NOTICE](NOTICE.md).

**Desktop shell and interface**

- [Tauri 2](https://tauri.app) (with the single-instance / dialog plugins) — service, window and WebView2 in one exe,
  plus "the second double-click brings the running window forward"
- [rust-embed](https://github.com/pyros2097/rust-embed) — the whole frontend goes into the exe, so it opens offline anywhere
- **Noto Serif SC / 思源宋体** (SIL OFL 1.1) — the Chinese serif used for headings and body text; upstream and licence in
  [`public/fonts/OFL.txt`](public/fonts/OFL.txt)

**Service and data**

- [axum](https://github.com/tokio-rs/axum) + [tokio](https://github.com/tokio-rs/tokio) — a localhost-only HTTP surface, and
  the blocking pool that carries the rule "heavy synchronous work always leaves the async worker"
- [rusqlite](https://github.com/rusqlite/rusqlite) (bundling public-domain SQLite) — projects, submission history and the
  parameter chain all live in one file
- [serde](https://github.com/dtolnay/serde) / [serde_json](https://github.com/dtolnay/serde_json),
  [thiserror](https://github.com/dtolnay/thiserror), [url](https://github.com/servo/rust-url),
  [pathdiff](https://github.com/manisheus/pathdiff), [futures-util](https://github.com/rust-lang/futures-rs) — the floor under
  parameters, errors and paths

**Image processing**

- [image](https://github.com/image-rs/image) + [zune-jpeg](https://github.com/etemesi254/zune-image) — decoding, and baking
  EXIF orientation into pixels (otherwise "the original looks upright, the thumbnail is lying on its side")
- [fast_image_resize](https://github.com/Cykooz/fast_image_resize) — resampling for the scaled levels, thumbnails and adjustment previews
- [imageproc](https://github.com/image-rs/imageproc) — mask dilation (that extra ring around your strokes before stitching)
- [moving-least-squares](https://github.com/mpizenberg/rust_mls) (MPL-2.0) — the MLS deformation maths the automatic face
  shaping would need. It's a dependency as-is, unmodified and not copied into this repo, so only its own files stay under MPL-2.0

**Transport and odds and ends**

- [reqwest](https://github.com/seanmonstar/reqwest) (with rustls / ring) — talking to local ComfyUI and cloud image APIs,
  with progress and interruption
- [base64](https://github.com/marshallpierce/rust-base64), [sha2](https://github.com/RustCrypto/hashes),
  [rand](https://github.com/rust-random/rand) — images in and out, asset fingerprints, random seeds
