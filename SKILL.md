---
name: html-video
description: 文章 URL → 粗剪解说视频。抓正文与配图、组件搭 slides、逐页演讲稿、分句、克隆配音、裁剪静音、合成不带字幕的 MP4。两个人审断点与断点续跑。
disable-model-invocation: true
---

# html-video：文章 URL → 解说视频

一条流水线，十步，两个人工断点。所有中间产物落盘，任何一步失败都能从断点续跑。整条流水线由一个工具包办：`$HV`（`html-video-toolbox`）的 `screenshot` / `state` / `check` / `pages` / `tts` / `compose` / `verify` 子命令。

**定位：粗剪。** 分句按语义走，不为了短视频节奏硬切；成片不烧字幕，字幕文件交给用户在剪辑器里自己合成和精修。

**耗时预期**：VoxCPM2 在本机 CPU 上加载约 1.5–2 分钟，每句合成约 1–2 分钟。10 分钟成片按 1–2 小时估。先合成 1–2 句试听，再跑全量。

| 步骤 | 产物 | 谁做 |
|---|---|---|
| `fetch` | `url`、`article.md`、`title.md`、`images/` | agent |
| `slides` | `index.html`、`slides/slide-01.png …` | agent + `screenshot` |
| `script` | `script.md` | agent（每页一段演讲稿） |
| `review-script` | 无（人工断点） | 人 |
| `speech` | `speech.md`、`slides/pages.json` | agent（分句）+ `pages` |
| `review-speech` | 无（人工断点） | 人 |
| `tts-text` | `speech.tts.md` | agent（LLM 转口语稿） |
| `tts` | `voice/`、`subs/final.srt` | `tts` |
| `compose` | `final.mp4`、`final.srt` | `compose`（ffmpeg） |
| `verify` | 无（验收报告） | `verify` |

## 目录

```text
<slug>/
├── url                    # 一行原文 URL
├── article.md             # 原文正文，保留原文语言，无标题/导航/推荐阅读
├── title.md               # 文章标题（可译成中文）
├── images/                # 配图 img-01.jpg …（无图不建）
├── script.md              # 每页一段演讲稿（页数 = slides 数）
├── index.html             # 960×540 幻灯片（组件搭积木）
├── slides/                # slide-01.png … 1920×1080
│   └── pages.json         # 句子 → 页 映射
├── speech.md              # 分句后的中文口播稿，每页一段，阿拉伯数字
├── speech.tts.md          # 配音稿，数字转中文读法，行与 speech.md 一一对应
├── voice/
│   ├── raw/               # 模型原始输出 sent-0001.wav
│   ├── sent-0001.wav      # 裁剪静默后
│   ├── full.wav           # 拼接整轨
│   ├── timeline.json      # 每句起止时间
│   └── manifest.json      # 每条语音对应的原文 + 配音稿
├── subs/final.srt
├── final.mp4
├── final.srt              # 外挂字幕
└── progress.json          # 断点状态
```

`$SKILL` = 本文件所在目录。`<slug>` 是文章目录路径，工具在任意目录都能执行；`mkdir` / `curl` 这类命令在文章目录里执行。

`$HV` 解析顺序（每次开工先用 `command -v html-video-toolbox` 确定一次）：系统 PATH 里的 `html-video-toolbox` → `$SKILL/tools/html-video-toolbox/target/release/html-video-toolbox` → 都没有才进依赖里的构建步骤。

## 依赖

```bash
python3 --version            # 不需要 python 脚本，仅用于临时处理
ffmpeg -version | head -1    # 需要 libx264 + aac
ffprobe -version | head -1
fc-match -f '%{family[0]}' 'sans-serif:lang=zh-cn'
ls ~/models/VoxCPM2/config.json
ls $SKILL/assets/cloned-voice.wav
ls $HV
```

- 工具不在 "$HV" 解析顺序的任何位置时才构建（首次约 10 分钟，含 burn/wgpu）：

  ```bash
  cd $SKILL/tools/html-video-toolbox && cargo build --release
  ```

- 截图后端是 headless Chromium，用 `CHROME_PATH` 或 `--chrome-path` 指定；找不到会直接报错，**不要再退回 firefox / playwright 截图**。没有浏览器就先装，或者让用户提供路径。
- 字体：`$SKILL/assets/fonts/` 内置 Noto Sans CJK SC / Mono / Serif，生成 deck 时复制到 `<slug>/fonts/`；缺失时回落系统字体，不阻断。
- 音色：`$SKILL/assets/cloned-voice.wav` 必须存在，**缺失就停**，提示用户补充后继续。
  `assets/cloned-voice.txt` 是对应参考文本，只供人工核对，VoxCPM2 不读取。
- 模型：默认 `~/models/VoxCPM2`，可用 `--model-dir` 或 `VOXCPM_MODEL_DIR` 覆盖。
- 路径优先级统一为：命令行参数 > 环境变量 > 默认。

## 断点协议

**每次开工先跑：**

```bash
$HV state status <slug>
```

输出里的 `next` 就是当前该做的步骤，`paused` 非空表示在等人。完成一步立刻：

```bash
$HV state done <slug> <step>
```

- 只允许按顺序完成；前置未完成会拒绝。
- 产物被删掉时状态自动退回 `pending`。
- 回退：`$HV state reset <slug> [step]`，从该步起全部置为未完成（不删产物）。
- `review-script` / `review-speech` 只能人工确认后手动 `done`；`review-speech` 未 `done` 时 `tts` 拒绝运行。
- 自检工具本身：`$HV selftest`（不需要模型与浏览器）。

## 0 · fetch

```bash
mkdir -p <slug>
```

1. 用 `fetch_content` 抓 URL，得到 markdown。
2. `article.md` 逐字保留原文正文（**不翻译**）：删掉大标题、导航、作者卡片、推荐阅读、版权声明、评论区；保留段落顺序与空行分段。
3. `title.md` 写一行标题，除了特定名词，必须译成中文；`url` 写一行 URL。
4. 正文里的图片按出现顺序下载到 `images/`：

```bash
mkdir -p images
curl -L --fail -A 'Mozilla/5.0' -o images/img-01.jpg '<图片 URL>'
```

如果直连现在失败，可以尝试使用 `Socks5` 代理重新下载一次，代理地址：`127.0.0.1:1084`
403/404 的图跳过，不要为了一张图卡住；`article.md` 里不要留图片语法。

5. `$HV state done <slug> fetch`

## 1 · slides

用 `$SKILL/template/` 里的组件搭 `index.html`，视觉与 `html-ppt` 的非代码模板保持一致：

1. 读 `$SKILL/template/COMPONENTS.md`，按内容选组件
2. `cp $SKILL/template/skeleton.html <slug>/index.html && cp -r $SKILL/assets/fonts <slug>/fonts`（字体随 index.html 的相对路径 `fonts/OOO.ttc` 加载，避免依赖系统字体；不复制也能跑，只是回落系统字体）
3. 把 `$SKILL/template/components/*.html` 的 `<section>` 按叙事顺序粘进去，替换文案与序号；关键数字/结论包 `<span class="hl">`

**必须为全文生成全部页面**，8–14 页（短文 5 页起），第一页 `cover`、最后一页 `closing`，不允许只生成前几页。配图用相对路径 `images/img-XX.jpg`。不要写页面局部样式，只用 `base.css` 里的类。每页必须是 `class="slide"` 的 `<section>`，`skeleton.html` 自带的居中样式、←→ 键盘翻页与幻灯片下方圆点页码脚本不要删。关键信息（数字、结论、核心术语）用 `<span class="hl">` 打品牌色，每页 1–3 处；图文页正文区自动铺满，图片随剩余高度伸缩。

截图（1920×1080）：

```bash
$HV screenshot <slug>/index.html -o <slug>/slides
```

- `-r 1080` 是默认档位：长边 = 档位 × 16 / 9 = 1920px，短边按页面比例。
- 产物 `slides/slide-01.png …`，数量与页数一致；只截部分页用 `-n 1-3`。
- 浏览器路径：`--chrome-path /path/to/chromium` 或 `CHROME_PATH`。

**截图后必须逐张自查**：用 `read` 打开 `slides/slide-*.png` 每一张，确认 ① `<span class="hl">` 高亮颜色正常显示（不是黑/白/透明、同一页不混用两种高亮色）；② 布局没塌——无文字溢出、无重叠、无被裁切、无大片空白、图片没变形、标题与正文没贴边。任何一张不合格就回改 `index.html`，重跑 `$HV screenshot`，直到全部通过。不许凭记忆认为「应该没问题」。

这一步**不写** `pages.json`，它要等分句完成（文件名零填充由 `pages` / `compose` 统一处理）。

`$HV state done <slug> slides`

## 2 · script

按 slides 的页数写 `script.md`：一页一段演讲稿，段前用 `## NN 标题` 标记。

- `##` 标题数量必须等于 `slides/slide-*.png` 数量，顺序与页面一致
- 每页一段口播稿，可以多句；不写分点、不写舞台提示；**不要使用破折号**
- 内容来自 `article.md`：原文是外文就用中文写，数字保留阿拉伯形式
- 不要跨页讲同一件事；一页只承担一个信息
- 纯封面页也要写一句开场白，不要留空

示例（直接写正文，不要包代码块）：

    ## 01 权重空间编程的星火
    前沿大模型正在获得一种新能力：不训练，直接读写神经网络的权重。

    ## 02 大模型直接读写权重
    研究者只看原始权重，就能说出一个网络被训练来做什么。

校验：

```bash
$HV check script <slug>
```

`ok: true` 要求 `pages` 与 `slides` 数量一致、每页都有正文。

`$HV state done <slug> script`

## 3 · review-script（人工断点）

**停在这里，不要继续分句。** 把 `script.md` 交给用户审查：

- 每页是否对应正确的 slide
- 有没有漏页、重复、跨页
- 事实、数字、语气是否要改

**用户确认后，先重新读取 `script.md` 最新内容**（用户可能直接改过文件），再：

```bash
$HV state done <slug> review-script
```

## 4 · speech

把 `script.md` 逐页分句，写 `speech.md`：

- 每页一段，段间空行；段落数必须等于 `script.md` 的页数
- 一行一句；`，`、`；`、`：`、`。`、`！`、`？` 皆是断点（顿号不是，保留短语连读）；每行以标点结尾
- 每行内容只增断行、不改字，拼接后与 `script.md` 对应段逐字一致；不强制字数上限
- 不让分句跨页；数字保持阿拉伯形式；句末标点保留
- 不要产生只含标点的句子

然后生成 `pages.json`：

```bash
$HV pages <slug>
```

工具按 `script.md` 的页标题和 `speech.md` 的段落边界生成 `slides/pages.json`，并校验 script 页数、speech 段落数、slides 图片数三者一致。

校验：

```bash
$HV check speech <slug>
```

`ok: true` 才算过。`mode` 为 `same-language` 时 `error` 表示拼不回原文；`mode` 为 `translated` 时只检查中文。`longest` 只是参考，不阻断。

**完成即停，不要继续 `tts-text`。** 下一步是人工审查。

`$HV state done <slug> speech`

## 5 · review-speech（人工断点）

**停在这里，不要继续配音。** 把 `speech.md` 交给用户审查：

- 分句是否自然、有没有断在奇怪的位置
- 译文是否准确、有没有漏译或错译
- 数字是否保留阿拉伯数字、`pages.json` 的页边界是否合理

**用户确认后，先重新读取 `speech.md` 最新内容**（用户可能直接改过；若改了，重跑 `$HV pages` 与 `check speech`），再：

```bash
$HV state done <slug> review-speech
```

用户要求改，就改 `script.md` / `speech.md`，重跑 `pages` 和 `check speech`，不要跳过任何一个 review。

## 6 · tts-text

把 `speech.md` 逐行转成口语稿 `speech.tts.md`：

- 行数、顺序、段落空行与 `speech.md` **完全一致**
- `##` 标题行不进语音、不进字幕（工具按行数对齐时自动跳过），保留原样即可，不需数字转换
- 每行去掉行尾句读标点（`。，；：、！？`），TTS 读起来自然；顿号在行中间照常保留
- 阿拉伯数字、年份、小数、百分比、金额、单位、版本号改成中文读法
- 示例：`2024 年` → `二零二四年`；`12.5%` → `百分之十二点五`；`3 km` → `三公里`；`1.5 亿` → `一点五亿`；`GPT-4` → `GPT 四`
- 不改语义、不增删内容；拿不准的按最自然的读法写

校验：

```bash
$HV check tts-text <slug>
```

`digits_left` 必须为空，`paragraph_mismatch` 必须为空。

`$HV state done <slug> tts-text`

## 7 · tts

```bash
$HV tts <slug> --gui --voice $SKILL/assets/cloned-voice.wav
```

可选：`--model-dir <dir>`、`--gpu`（用 wgpu GPU 后端合成，不加则默认 CPU `flex` 后端）、`--force`（全量重合成）、`--only 1,3,5`（只处理这几句）、`--cfg` / `--steps` / `--max-len`（生成参数）。工具一次加载模型，逐句合成，裁剪每句首尾静默（句首 ≤50ms、句尾 ≤120ms，-40dB），按句间 150ms、段间 400ms 拼接，写出 `voice/full.wav`、`voice/timeline.json`、`voice/manifest.json`、`subs/final.srt`。

- 音色缺失会直接报错停下，补好 `assets/cloned-voice.wav` 后重跑同一条命令（或设 `VOICE_CLONE_WAV`）。
- 断点粒度是句子：`speech.tts.md` 某行不变就跳过该句；只改了几句就只重合成那几句。
- `manifest.json` 记录了每条语音对应的原文与配音稿，可逐条核对。

`$HV state done <slug> tts`

## 8 · compose

```bash
$HV compose <slug>
```

按 `pages.json` 每页图显示到「下一页第一句开始前 200ms」，首页从 0、末页到音轨结束；挂 `voice/full.wav`，导出**不带字幕**的 `final.mp4`，同时把 `subs/final.srt` 复制成 `final.srt`。用户随后在剪辑器里自行导入字幕并精修。

`$HV state done <slug> compose`

## 9 · verify

```bash
$HV verify <slug>
```

必须 `ok: true`。硬检查：页覆盖完整、字幕条数与句子数一致且文本等于 `speech.md`、音轨与成片时长差 < 0.5s。逐句语速异常只进 `warnings`，不阻断。

`$HV state done <slug> verify`

## 硬规则

- `script.md` 一页一段，页数 = slides 数；`speech.md` 段落数 = `script.md` 页数。
- 字幕文本永远来自 `speech.md`（中文、阿拉伯数字），行尾句读标点由 `tts` 自动去掉（屏幕上拖尾标点很突兀）；`speech.tts.md` 只给 TTS 读，必须保留供排错。
- `article.md` 保留原文语言，不翻译；翻译只发生在 `speech.md`。
- 不改原文、不编内容；分句校验不过就不许进配音。
- 页时间只能落在句边界上，不按字数估算。
- 成片不烧字幕；`final.srt` 是给用户后期用的，不要在 ffmpeg 里加 `subtitles`。
- 截图只走 `$HV screenshot`，不许换浏览器、不许用 playwright 兜底。
- 截图后必须逐张看图确认高亮颜色与布局；有不合格的就改 `index.html` 重截，不许直接进 `script`。
- 这是粗剪：允许句子偏长、允许停顿不完美，精修交给剪辑器。
- 任何一步失败先看 `$HV state status`，不要从头重跑。

## 故障处理

| 现象 | 处理 |
|---|---|
| `找不到 Chromium/Chrome` | 装 Chromium，或用 `--chrome-path` / `CHROME_PATH` 指定 |
| `第 N 页尺寸为 0` | `index.html` 里没有 `class="slide"`，或页面结构被改坏 |
| `缺少克隆音色参考音频` | 把参考音频放到 `$SKILL/assets/cloned-voice.wav`，或 `--voice` 指定 |
| `模型目录无效` | `--model-dir ~/models/VoxCPM2` 或设 `VOXCPM_MODEL_DIR` |
| `review-speech 未完成` | 先人工审完 `speech.md`，再 `$HV state done <slug> review-speech` |
| `分句拼不回 article.md` | 仅同语言时出现：`article.md` 混进了标题/导航，或分句时改了字 |
| `有句子不是中文` | `speech.md` 漏译或出现了纯标点行 |
| `digits_left` 非空 | `speech.tts.md` 还有阿拉伯数字，改中文读法 |
| `pages.json 不连续` | 重跑 `$HV pages <slug>`，别手改 from/to |
| `script.md 页数与 slides 不一致` | 补页或删页，重跑 `$HV check script` |
| `speech.md 段落数与 script.md 不一致` | 按页重新分句，重跑 `$HV pages` |
| `ffmpeg 合成失败` | 看 stderr；多半是图片路径或 concat 列表 |
| 合成太慢 | 正常：CPU 推理 4.3GB 模型；已合成的句子不会重跑 |
