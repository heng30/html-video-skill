---
name: html-video
description: 文章 URL → 粗剪解说视频。抓正文与配图、组件搭 slides、逐页演讲稿、分句、克隆配音、裁剪静音、合成不带字幕的 MP4。两个人审断点与断点续跑。
disable-model-invocation: true
---

# html-video：文章 URL → 解说视频

一条流水线，十步，两个人工断点。所有中间产物落盘，任何一步失败都能从断点续跑。整条流水线由一个工具包办：`$HV`（`html-video-toolbox`）的 `screenshot` / `state` / `check` / `pages` / `tts` / `asr` / `compose` / `verify` 子命令。

**定位：粗剪。** 分句按语义走，不为了短视频节奏硬切；成片不烧字幕，字幕文件交给用户在剪辑器里自己合成和精修。

**耗时预期**：VoxCPM2 在本机 CPU 上加载约 1.5–2 分钟，每句合成约 1–2 分钟。10 分钟成片按 1–2 小时估。先合成 1–2 句试听，再跑全量。回听校验（`asr`）另算：模型加载约 20 秒，每句再多几秒。

**人工审核点铁律**：每个审核点（`review-script`、`review-speech`）用户说"看过了/没问题"之后，**第一步必须用 `read` 重新完整读取被审文件**（`script.md` / `speech.txt`），以磁盘上的内容为准；不许用上下文里的旧副本，不许只凭"用户说没问题"就直接往下走。用户很可能直接改了文件——不重读 = 审核白做，下游全按旧内容生产。

**审核产物只读铁律**：用户审核过的文件（`script.md`、`speech.txt`）在审核点之后**一个字都不许改**——包括为了让 `$HV check` 变绿而重新断行、补标点、改标点、调语序。**审核完也无需再校验**：`check script` / `check speech` 只在 agent 写完时跑一次，人工改过的断句以人的判断为准，重跑只会报出无意义的告警。要改只能改用户没审的下游文件（`speech.tts.txt`、`index.html`、`cover.html`）。

**`tts` / `asr` 一律丢后台跑**（`background_bash`），别占着前台等；启动后用 `read_process_output` 轮询进度，等日志出现完成标志或进程退出再看结果。

| 步骤 | 产物 | 谁做 |
|---|---|---|
| `fetch` | `url`、`article.md`、`title.md`、`images/` | agent |
| `slides` | `index.html`、`slides/slide-01.png …` | agent + `screenshot` |
| `covers` | `covers/cover-16x9.png`、`cover-4x3.png`、`cover-3x4.png` | agent + `screenshot --covers` |
| `script` | `script.md` | agent（每页一段演讲稿） |
| `review-script` | 无（人工断点） | 人 |
| `speech` | `speech.txt`、`slides/pages.json` | agent（分句）+ `pages` |
| `review-speech` | 无（人工断点） | 人 |
| `tts-text` | `speech.tts.txt` | agent（LLM 转口语稿 + 数字读法二次检查） |
| `tts` | `voice/`、`subs/final.srt`、`voice/asr.json` | `tts` + `asr`（逐句回听校验） |
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
├── cover.html             # 三张平台封面（从 template/cover.html 复制）
├── covers/                # cover-16x9.png / cover-4x3.png / cover-3x4.png（`slides` 步的必备产物）
├── speech.txt              # 分句后的中文口播稿（纯文本），每页一段，阿拉伯数字
├── speech.tts.txt          # 配音稿（纯文本），数字转中文读法，行与 speech.txt 一一对应（agent 自己二次检查读法，不入人工审）
├── voice/
│   ├── raw/               # 模型原始输出 sent-0001.wav
│   ├── sent-0001.wav      # 裁剪静默后
│   ├── full.wav           # 拼接整轨
│   ├── timeline.json      # 每句起止时间
│   ├── manifest.json      # 每条语音对应的原文 + 配音稿
│   ├── asr.json           # 逐句转录结果：原文 / 转录 / 相似度 / 是否通过
│   ├── asr-retry.json     # 待重新合成的句子，通过校验后自动移除
│   └── bad/               # 转录不一致的音频存档（排错用，不自动删）
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
ls ~/models/Fun-ASR-Nano-2512/model.pt ~/models/Fun-ASR-Nano-2512/Qwen3-0.6B/tokenizer.json
ls $SKILL/assets/cloned-voice.wav
ls $HV
```

- 工具不在 "$HV" 解析顺序的任何位置时才构建（首次约 10 分钟，含 burn/wgpu 与 candle）：

  ```bash
  cd $SKILL/tools/html-video-toolbox && cargo build --release
  ```

- 截图后端是 headless Chromium，用 `CHROME_PATH` 或 `--chrome-path` 指定；找不到会直接报错，**不要再退回 firefox / playwright 截图**。没有浏览器就先装，或者让用户提供路径。
- 字体：`$SKILL/assets/fonts/` 内置 Noto Sans CJK SC / Mono / Serif，生成 deck 时复制到 `<slug>/fonts/`；缺失时回落系统字体，不阻断。
- 音色：`$SKILL/assets/cloned-voice.wav` 必须存在，**缺失就停**，提示用户补充后继续。
  `assets/cloned-voice.txt` 是对应参考文本，只供人工核对，VoxCPM2 不读取。
- TTS 模型：默认 `~/models/VoxCPM2`，可用 `--model-dir` 或 `VOXCPM_MODEL_DIR` 覆盖。
- 回听模型（Fun-ASR-Nano，candle）：默认 `~/models/Fun-ASR-Nano-2512`，可用 `--model-dir` 或 `FUNASR_MODEL_DIR` 覆盖。`$HV` 内置该模型代码（`lib/fun-ast-nano`），缺失权重就直接报错，**不要拿别的 ASR 顶替**。
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
4. 要贴代码就用 `code.html`（纯代码页）或 `code-image.html`（代码 + 输出图）：一页 12–16 行，`<pre><code class="language-rust">` 里的 `&` 和 `<` 写成 `&amp;` / `&lt;`，行号由 `skeleton.html` 脚本自动加，不要手写；全篇最多两三页

**必须为全文生成全部页面**，8–14 页（短文 5 页起），第一页 `cover`、最后一页 `closing`，不允许只生成前几页。配图用相对路径 `images/img-XX.jpg`。不要写页面局部样式，只用 `base.css` 里的类。每页必须是 `class="slide"` 的 `<section>`，`skeleton.html` 自带的居中样式、←→ 键盘翻页与幻灯片下方圆点页码脚本不要删。关键信息（数字、结论、核心术语）用 `<span class="hl">` 打品牌色，每页 1–3 处；图文页正文区自动铺满，图片随剩余高度伸缩。

代码页的语法着色靠 `skeleton.html` 里的 hljs CDN（`highlight.min.js`）；没网时自动退化成纯文本代码块，行号和布局不变，**不要因为本地预览没颜色就改成手写 `<span>` 上色**。

截图（1920×1080）：

```bash
$HV screenshot <slug>/index.html -o <slug>/slides
```

- `-r 1080` 是默认档位：长边 = 档位 × 16 / 9 = 1920px，短边按页面比例。
- 产物 `slides/slide-01.png …`，数量与页数一致；只截部分页用 `-n 1-3`。
- 浏览器路径：`--chrome-path /path/to/chromium` 或 `CHROME_PATH`。

**截图后必须逐张自查**：用 `read` 打开 `slides/slide-*.png` 每一张，确认 ① `<span class="hl">` 高亮颜色正常显示（不是黑/白/透明、同一页不混用两种高亮色）；② 布局没塌——无文字溢出、无重叠、无被裁切、无大片空白、图片没变形、标题与正文没贴边。任何一张不合格就回改 `index.html`，重跑 `$HV screenshot`，直到全部通过。不许凭记忆认为「应该没问题」。

这一步**不写** `pages.json`，它要等分句完成（文件名零填充由 `pages` / `compose` 统一处理）。

**平台封面（必须做）**：用 `$SKILL/template/cover.html` 同一套视觉出三张。`covers/` 是 `slides` 步的必备产物，缺了 `$HV state done <slug> slides` 会一直不生效：

```bash
cp $SKILL/template/cover.html <slug>/cover.html
$HV screenshot <slug>/cover.html --covers -o <slug>/covers
```

- 产物 `covers/cover-16x9.png`（1920×1080）、`covers/cover-4x3.png`（1440×1080）、`covers/cover-3x4.png`（1080×1440），按短边 1080 出图。三个 `<section class="slide">` 的宽高比必须是 16:9 / 4:3 / 3:4，改尺寸或删页都直接报错。
- 只改三个 `<section>` 里的文案，不动 `cover.html` 的布局样式。
- **居中硬规则**：kicker、标题、导语、分隔线、署名这整块内容，在画面里必须**水平居中且垂直居中**。不许偏左、不许压底、不许改成左对齐或上下分栏。截完逐张 `read` 看图，确认居中、无溢出、`<span class="hl">` 颜色正常；不合格就改文案或换更短的标题重截。
- 封面是三张，缺一张都不算完成；不是断点步骤，不单独 `state done`，但必须先出完三张才能 `state done <slug> slides`。

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

**写完必须逐段重读一遍做通顺检查（不能省）**，`check script` 只查页数与空段，查不出语病：

1. 断句与搭配：`收敛进最优`、`避免 A 和 B` 这种一半对一半错的搭配；动宾、主谓是否对得上。
2. 主语与句式：被动句里插主动动词、缓兵句里换主语（`它连训练循环都不被允许写`）。
3. 口头不顺的短语：`四猜四中`、`零数据地`、`被…来…` 这类写得出、念不出的。
4. 标点逻辑：冒号后面该是解释而不是对比一前一后两个动作；逗号后面不能是悬空同位语。
5. 指标引用是否指得清：`第 10 层第 2 号注意力头` 而不是 `十号第二头`。

逐段列出问题 → 改 → 重跑 `$HV check script`。改完脚本要连带重做下游：`speech.txt` 重新分句 → `pages` → `check speech` → `speech.tts.txt` → `check tts-text`。

`$HV state done <slug> script`

## 3 · review-script（人工断点）

**停在这里，不要继续分句。** 把 `script.md` 交给用户审查：

- 每页是否对应正确的 slide
- 有没有漏页、重复、跨页
- 事实、数字、语气是否要改

**用户确认后，第一步：用 `read` 重新读取整个 `script.md`**（不要用上下文里的旧版本，也不要只看用户贴的片段）。读到的内容与上下文不一致时，以文件为准，**不要重跑 `$HV check script`**（审核后不再校验被审文件），直接同步下游（`speech.txt` → `pages` → `speech.tts.txt` → `check tts-text`）。

确认与磁盘内容一致后，再：

```bash
$HV state done <slug> review-script
```

## 4 · speech

把 `script.md` 逐页分句，写 `speech.txt`：

- 每页一段，段间空行；段落数必须等于 `script.md` 的页数
- 一行一句，**分两级切**：
  1. 先在句末标点断（`。`、`！`、`？`、`……`），得到完整的一句话；行内不许再出现句末标点
  2. 这一句超过 **20 个字符**时，才在这句里面找次级断点（`，`、`；`、`：`、破折号 `—`；顿号 `、` 不算，它只分隔词组）切一次；优先选靠中间的断点，切开后两边都至少 6 字符
  3. 超过 20 字符但找不到合适的次级断点，就整句留着，不要为了凑长度硬切、也不要切出「但是，」这种碎片
- 每行以标点结尾；只增断行、不改字，拼接后与 `script.md` 对应段逐字一致
- 不让分句跨页；数字保持阿拉伯形式；句末标点保留
- 不要产生只含标点的句子

然后生成 `pages.json`：

```bash
$HV pages <slug>
```

工具按 `script.md` 的页标题和 `speech.txt` 的段落边界生成 `slides/pages.json`，并校验 script 页数、speech 段落数、slides 图片数三者一致。

校验：

```bash
$HV check speech <slug>
```

`ok: true` 才算过。`mode` 为 `same-language` 时 `error` 表示拼不回原文；`mode` 为 `translated` 时只检查中文；`sentence split does not follow the two-level rule` 表示分句层次不对（`split_at_sentence_end` = 该在句末标点拆、`split_at_clause_break` = 超长该在逗号类断点二次拆）。`longest` 只是参考，不阻断。

**完成即停，不要继续 `tts-text`。** 下一步是人工审查。

`$HV state done <slug> speech`

## 5 · review-speech（人工断点）

**停在这里，不要继续配音。** 把 `speech.txt` 交给用户审查：

- 分句是否自然、有没有断在奇怪的位置
- 译文是否准确、有没有漏译或错译
- 数字是否保留阿拉伯数字、`pages.json` 的页边界是否合理

**用户确认后，第一步：用 `read` 重新读取整个 `speech.txt`**（不要用上下文里的旧版本）。内容有变就以文件为准：重跑 `$HV pages`，并**重做 `speech.tts.txt`**（它逐行对应 `speech.txt`，行数和行序都会变），再 `$HV check tts-text`。**不要再跑 `$HV check speech`**：审核版已定，工具的两级切分规则只是建议，重跑只会报出人工判断过的“错误”。

确认与磁盘内容一致后，再：

```bash
$HV state done <slug> review-speech
```

用户要求改，就改 `script.md` / `speech.txt`，重跑 `pages`，不要跳过任何一个 review。改动后同样不对被审文件跑 `check script` / `check speech`。

## 6 · tts-text

把 `speech.txt` 逐行转成口语稿 `speech.tts.txt`：

- 行数、顺序、段落空行与 `speech.txt` **完全一致**
- `##` 标题行不进语音、不进字幕（工具按行数对齐时自动跳过），保留原样即可，不需数字转换
- 每行去掉行尾句读标点（`。，；：、！？`），TTS 读起来自然；顿号在行中间照常保留
- 阿拉伯数字、年份、小数、百分比、金额、单位、版本号改成中文读法
- 不改语义、不增删内容；拿不准的按最自然的读法写

**先定读法，再动笔。** 同一个数字有两种读法，先看它在句子里指什么：

| 数字是 | 读法 | 例子 |
|---|---|---|
| 数量 / 金额 / 时长 / 比例 | **按数值**：二十、一百零五、三千五百万 | `20 人` → `二十人`；`105 次` → `一百零五次`；`3000 万` → `三千万` |
| 年份 / 编号 / 门牌 / 页码 / 型号 | **逐位**：二零二四、一九九零、一零五、GPT 四 | `2024 年` → `二零二四年`；`1990 年代` → `一九九零年代`；`GPT-4` → `GPT 四` |
| 小数 | 点前按数值、点后逐位 | `3.14` → `三点一四`；`10.5 亿` → `十点五亿` |
| 百分比 / 分数 | `%` → 百分之 + 数值；`a/b` → b 分之 a | `12.5%` → `百分之十二点五`；`1/3` → `三分之一` |
| 序数 | 第 + 读法（用二，不用两） | `第 2 章` → `第二章`；`第 20 条` → `第二十条` |
| 量词前的 2 | 用「两」 | `2 个人` → `两个人`；`2 年` → `两年` |

最典型的两个错：`20` 写成 `二零`（该是 `二十`）、`105` 写成 `一百五` 或 `一零五`（该是 `一百零五`）；另外别丢单位：`3000 万` 是 `三千万`，`3 km` 是 `三公里`。

**英文单词与标识符按形状分三类写，不要一律照抄：**

| 形态 | 写法 | 例子 |
|---|---|---|
| 全大写但小写后是个正常词 | **转小写**，按普通单词读 | `NEXT` → `next`；`ONCE` → `once`；`RUST` → `rust`；`ALL` → `all` |
| 缩写，但字母能拼成一个词 | **照原样保留**，整体当一个词读，不要拆成单个字母 | `JSON` → `JSON`；`APP` → `APP`；`RAM` → `RAM` |
| 专有名称 / 缩写，只能逐字母念 | **逐字母写开，字母间空格分隔**，不合并成词、不自然拼读 | `API` → `A P I`；`GPU` → `G P U`；`HTTP` → `H T T P`；`SDK` → `S D K`；`LLM` → `L L M`；`XML` → `X M L` |
| 函数名 / 变量名 / 驼峰名（带缩写） | **先还原缩写原词，再按单词、驼峰、连字符拆开** | `getCfg` → `get config`；`parseReqBody` → `parse request body`；`initDB` → `initialize D B`；`useMemo` → `use memo`；`srcDir` → `source directory`；`VoidZero` → `void zero`；`napi-rs` → `N API RS` |

- 缩写还原靠猜：`cfg` config、`req` request、`res` response、`ctx` context、`fn` function、`impl` implementation、`msg` message、`err` error、`arg` argument、`idx` index、`len` length、`init` initialize、`tmp` temporary、`str` string、`num` number、`env` environment、`dir` directory、`pkg` package、`repo` repository、`lib` library、`deps` dependencies、`auth` authentication。猜不准就按音节拆开写（`nginx` → `engine x`)，不要留一坨驼峰让 TTS 自己拼。
- 专有名同样先拆再考虑音译：`VoidZero` → `void zero`、`napi-rs` → `N API RS`、`oxc` → `O X C`、`ts.Transformer` → `T S transformer`。拆完还读不准，才按 §7 换成中文音译。

**写完必须做一次二次检查（这一条不能省）：** 把 `speech.txt` 和 `speech.tts.txt` 并排逐行读，**只看带数字和英文的行**：数字每处过三问——① 它在这里是数量、序数还是编号/年份，读法选对了吗？② 位数丢了吗（有零位没读「零」）？③ 量词、单位、万/亿还挂着吗？英文每处过一句——大写转小写了吗、缩写展开成原词了吗、只能逐字母的有没有写开（`JSON` / `APP` / `RAM` 这类能拼成词的缩写不拆）？

- 把改过的数字逐条列出来（原文 → 读法 → 类型），有疑点的句子单独重写这一行，**不要整篇重写**（会破坏与 `speech.txt` 的行对齐）。
- 只做了阿拉伯数字清理、没做英文标识符整理或二次检查，视为这一步没做完。工具只查得出「还有阿拉伯数字」，查不出「二零」写错，也查不出 `api` 被拼成词。

校验：

```bash
$HV check tts-text <slug>
```

`digits_left` 必须为空，`paragraph_mismatch` 必须为空；改过数字后重跑一次。

`$HV state done <slug> tts-text`

## 7 · tts

```bash
$HV tts <slug> --gpu --voice $SKILL/assets/cloned-voice.wav
```

**必须先用 `--gpu` 跑；日志出现 wgpu 初始化/加载失败或进程报错退出时，才去掉 `--gpu` 回退 CPU 重跑同一条命令**（不加 `--gpu` 即默认 CPU `flex` 后端）。成功标志是日志出现 `loading model ... on wgpu/gpu`，随后逐句 `[N/M]`；回退 CPU 时对应 `on flex/cpu`。

命令耗时以十分钟计，**丢后台跑**，不要前台阻塞。

**开跑前必须确认 `slides/pages.json` 是最新的**：页间 800ms 静默的位置由它决定（不是 `speech.txt` 的段落）。`$HV pages` 必须在最终版 `speech.txt` 定稿后跑过，`pages.json` 最后一条的 `to` 要等于句子总数。`pages.json` 比 `speech.txt` 旧，800ms 就会落在错的句子上，`compose` 时翻页音效会叠进人声。

可选：`--model-dir <dir>`、`--force`（全量重合成）、`--only 1,3,5`（只处理这几句）、`--retry`（只重合成 `voice/asr-retry.json` 里的句子）、`--cfg` / `--steps` / `--max-len`（生成参数）、`--rate-tolerance`（语速离群重做阈值，默认 0.25，0 关闭）。工具一次加载模型，逐句合成，裁剪每句首尾静默（句首 ≤50ms、句尾 ≤120ms，-40dB），合成后按**每单位时长**中位数把语速偏离超过阈值的句子换噪声重做（最多 2 次，只留更接近中位的一次），按句间 150ms、段间 400ms、页间 800ms（给翻页音效留静默）拼接，音轨首尾各留静默（`--lead-in-ms` 默认 400、`--tail-ms` 默认 600，成片开场与结尾都先静一拍），写出 `voice/full.wav`、`voice/timeline.json`、`voice/manifest.json`、`subs/final.srt`。

**语速单位：一个汉字 = 一个英文单词 = 1**（`O X C` 算 3，`napi-rs` 算 2）。不要按字符数算：`VoidZero` 逐字符是 8 个字，一句里带几个英文标识符就会被判成语速异常，白白重合成半天。

- 音色缺失会直接报错停下，补好 `assets/cloned-voice.wav` 后重跑同一条命令（或设 `VOICE_CLONE_WAV`）。
- 断点粒度是句子：`speech.tts.txt` 某行不变就跳过该句；只改了几句就只重合成那几句。
- `manifest.json` 记录了每条语音对应的原文与配音稿，可逐条核对。

**合成完必须逐句回听校验，不能直接进 `compose`：**

```bash
$HV asr <slug>
```

工具用 Fun-ASR-Nano 逐句转录 `voice/sent-NNNN.wav`，和 `speech.tts.txt` 对应行比对（去掉标点、阿拉伯数字按位转中文读法后算相似度），写出 `voice/asr.json`。

- `--only 3,7`（逗号分隔句号）只转录这几句，不重跑全量；结果合并进 `voice/asr.json`，其余句子保留上次记录。**重查问题音频一律带 `--only`**，模型加载 20 秒一次，别为两句把整片重新转录一遍。
- 全部相似度 ≥ `--min-similarity`（默认 0.85）才 `ok: true`。
- 不一致的句子记进 `voice/asr-retry.json`，并把当前音频拷到 `voice/bad/sent-NNNN-<第几次>.wav` 存档排错，**这两样都不删**。
- 有 `mismatched` 时命令以非零退出，接着重合成，再回听，直到全部通过：

```bash
$HV tts <slug> --retry --voice $SKILL/assets/cloned-voice.wav
$HV asr <slug> --only <刚重合成的句号>
```

- 退出码看的是 `voice/asr-retry.json` 整体：`--only` 没覆盖到的旧问题句仍留在里面，照样非零退出，不会被悄悄放过。
- `--retry` 只重合成 `voice/asr-retry.json` 里的句子（自动 force）；重合成后再跑 `asr`，能对上就自动从文件里移除，`entries` 清空才算过。
- 首次全量 `asr` 之后，回环只跑单句 `--only`；只有 `speech.tts.txt` 大面积重写、或 `asr.json` 缺失时才重跑全量。

**先分类，再决定要不要重合成。** 相似度不达标不等于读错了。读 `voice/asr.json` 的 `asr_text` 和原句对比：

| 现象 | 结论 | 处理 |
|---|---|---|
| 中文部分逐字对得上，只有英文标识符/单词对不上 | **ASR 英文转写缺陷，不是 TTS 读错** | 不重合成。`$HV asr <slug> --accept <句号>` 把它从 `asr-retry.json` 里划掉 |
| 中文也听错、漏字、串行 | TTS 真的读糊了 | 改 `speech.tts.txt` 那一行的读法，再 `--retry` + `--only` 重查 |

```bash
$HV asr <slug> --accept 11,22     # 只接受这几句，其余仍按相似度判定
```

- **不要为了英文标识符改中文音译**（`napi-rs` → `纳皮尔斯`、`WASM` → `瓦森姆` 这类）。实测效果更差：TTS 念中文、字幕却仍是原文，且 ASR 的英文转写本来就不准，改了也过不了。
- `--accept` 只是把该句从待重做队列里移除，`voice/asr.json` 仍保留它的 `asr_text` 和 `similarity`，`accepted` 字段记下被接受的句号，事后能查。
- 中文确实读糊的：改那一行读法 → `check tts-text` → `$HV tts <slug> --retry` → `$HV asr <slug> --only <该句号>`。只改单行，不要整篇重写（会破坏与 `speech.txt` 的行对齐）。
- 反复仍不过才用 `--min-similarity` 放宽；不要直接 `state done tts` 蒙过去。
- 只有 `asr` 全绿（或剩下的句子全部 `--accept` 过、或人工逐条听过 `voice/bad/`）才能继续。

`$HV state done <slug> tts`

## 8 · compose

```bash
$HV compose <slug>
```

按 `pages.json` 每页图显示到「下一页第一句开始前 200ms」，首页从 0、末页到音轨结束（含 `tts` 留的 600ms 尾静默，画面停住再收尾）；挂 `voice/full.wav`，导出**不带字幕**的 `final.mp4`，同时把 `subs/final.srt` 复制成 `final.srt`。每次翻页（除首页外每页切点）叠一次 `assets/page-flip.mp3` 翻页音效（音量 `--flip-gain`，默认 0.5，0 关闭）：音效末尾对齐切页点、整段落在 `tts` 留的页间静默里，不盖人声；页间静默不足 0.55s 时报警告：先查 `slides/pages.json` 是不是最新的（`to` 等于句子总数、与 `voice/timeline.json` 的 800ms 位置对得上），不是就先 `$HV pages <slug>` 再重跑 `$HV tts`（**不加 `--force`**，句子没变就跳过合成，只重拼接、只改静默，几分钟完事）；确认 `pages.json` 没问题才考虑 `--force`。`--flip-gain` 调低只是把重叠变小，不算修好。音效内置在二进制里，混音写临时轨、不改 `voice/full.wav`，音轨总长与字幕时间轴不变（页间静默本来就写进 `timeline.json` 与 `subs/final.srt`）。用户随后在剪辑器里自行导入字幕并精修。

`$HV state done <slug> compose`

## 9 · verify

```bash
$HV verify <slug>
```

必须 `ok: true`。硬检查：页覆盖完整、字幕条数与句子数一致且文本等于 `speech.txt`、音轨与成片时长差 < 0.5s。逐句语速异常（同样按「一个汉字 = 一个英文单词 = 1」算，0.08–0.60 秒/单位）只进 `warnings`，不阻断。

`$HV state done <slug> verify`

## 硬规则

- `script.md` 一页一段，页数 = slides 数；`speech.txt` 段落数 = `script.md` 页数。
- 字幕文本永远来自 `speech.txt`（中文、阿拉伯数字），行尾句读标点由 `tts` 自动去掉（屏幕上拖尾标点很突兀）；`speech.tts.txt` 只给 TTS 读，必须保留供排错。
- `article.md` 保留原文语言，不翻译；翻译只发生在 `speech.txt`。
- 数字读法分两类：数量按数值（二十），年份/编号逐位（二零二四）；`speech.tts.txt` 写完必须做二次检查，按数字类型逐处核对读法。
- 不改原文、不编内容。`check script` / `check speech` **只在 agent 自己写完时跑一次**；一旦过了人工审核点，被审文件（`script.md` / `speech.txt`）**不再校验**——人工改过的断句优先于工具规则，重跑只会产出无用告警。
- 每个审核点结束后必须 `read` 重读被审文件，没重读就不许 `state done`、不许往下走。
- 审核过的 `script.md` / `speech.txt` 是只读的：不许为了让校验变绿而重新断行、补标点、改语序。要动手只能改用户没审的下游（`speech.tts.txt`、`index.html`、`cover.html`），`speech.tts.txt` 与 `pages.json` 必须按审核版重新生成。
- 页时间只能落在句边界上，不按字数估算。
- 成片不烧字幕；`final.srt` 是给用户后期用的，不要在 ffmpeg 里加 `subtitles`。
- 每句合成音频都要过 `$HV asr` 回听校验；重查只带 `--only` 跑出问题的那几句；`voice/asr-retry.json` 非空就不许 `state done tts`、不许 `compose`。英文标识符转录对不上属 ASR 缺陷，用 `--accept` 划掉，**不重合成、不改中文音译**。
- `voice/bad/` 里的存档是排错证据，校验通过也不删。
- 截图只走 `$HV screenshot`，不许换浏览器、不许用 playwright 兜底。
- 截图后必须逐张看图确认高亮颜色与布局；有不合格的就改 `index.html` 重截，不许直接进 `script`。
- 平台封面必做：`cover.html` + `covers/cover-16x9.png`、`cover-4x3.png`、`cover-3x4.png` 三张都要有，缺一张 `slides` 步就 `done` 不了；截完逐张看图确认居中。
- 这是粗剪：允许句子偏长、允许停顿不完美，精修交给剪辑器。
- 任何一步失败先看 `$HV state status`，不要从头重跑。

## 故障处理

| 现象 | 处理 |
|---|---|
| `找不到 Chromium/Chrome` | 装 Chromium，或用 `--chrome-path` / `CHROME_PATH` 指定 |
| `第 N 页尺寸为 0` | `index.html` 里没有 `class="slide"`，或页面结构被改坏 |
| `缺少克隆音色参考音频` | 把参考音频放到 `$SKILL/assets/cloned-voice.wav`，或 `--voice` 指定 |
| `模型目录无效` | `--model-dir ~/models/VoxCPM2` 或设 `VOXCPM_MODEL_DIR` |
| `invalid asr model directory` | 缺 `~/models/Fun-ASR-Nano-2512/model.pt` 或 `Qwen3-0.6B/tokenizer.json`，补齐或 `--model-dir` / `FUNASR_MODEL_DIR` |
| `sentences do not match the script` | 先看 `asr.json` 的 `asr_text`：只有英文对不上就用 `$HV asr <slug> --accept <句号>` 划掉；中文也听错才 `$HV tts <slug> --retry` 重合成，再 `$HV asr <slug> --only <报出的句号>` |
| `no sentence queued in voice/asr-retry.json` | 先跑 `$HV asr`，还没校验就没有待重合成的句子 |
| `review-speech 未完成` | 先人工审完 `speech.txt`，再 `$HV state done <slug> review-speech` |
| `sentence split does not follow the two-level rule` | 按两级切分重排：先把一行里多出来的 `。！？` 拆开（`split_at_sentence_end`），再把超过 20 字符、且有合适逗号类断点的句子拆开（`split_at_clause_break`，顿号不算） |
| `分句拼不回 article.md` | 仅同语言时出现：`article.md` 混进了标题/导航，或分句时改了字 |
| `有句子不是中文` | `speech.txt` 漏译或出现了纯标点行 |
| `speech.txt and speech.tts.txt line counts differ` | 违反审核点铁律：用户在 `review-speech` 改过 `speech.txt`，但没重读重做 `speech.tts.txt`。重读 `speech.txt` → 重做 `speech.tts.txt` → `check tts-text` |
| `digits_left` 非空 | `speech.tts.txt` 还有阿拉伯数字，改中文读法 |
| 数字读法念错（如 `20` 变 `二零`、`105` 变 `一百五`） | `check tts-text` 查不出来；回 §6 做二次检查，按数量/编号分类重写那几行，再重跑 `check tts-text` |
| `asr` 卡在英文标识符句（`napi-rs`、`WASM`、`ts.Transformer` 相似度低） | ASR 英文转写缺陷，不是读错，**不要重合成、不要改中文音译**；`$HV asr <slug> --accept <句号>` 划掉即可（中文部分对得上就不管英文） |
| `pages.json 不连续` | 重跑 `$HV pages <slug>`，别手改 from/to |
| `script.md 页数与 slides 不一致` | 补页或删页后重跑 `$HV check script`（仅限 agent 自己写的稿；审核后不再校验） |
| 脚本念着别扭、有语病 | `check script` 查不出来；回 §2 的通顺检查逐段重读，改完连带重做 `speech.txt` → `pages` → `speech.tts.txt` |
| `speech.txt 段落数与 script.md 不一致` | 按页重新分句，重跑 `$HV pages` |
| `compose` 报 `page turn ... has only 400ms of silence` | `slides/pages.json` 与 `voice/timeline.json` 不同步（通常是先 `tts` 后重跑 `pages`）。重跑 `$HV pages` → `$HV tts`（不加 `--force`，只重拼接）→ `$HV compose`；别用 `--flip-gain` 压低音效糊过去 |
| `ffmpeg 合成失败` | 看 stderr；多半是图片路径或 concat 列表 |
| `wgpu` 后端加载/初始化失败 | 去掉 `--gpu` 回退 CPU 重跑同一命令；已合成的句子不重跑 |
| 合成太慢 | 正常：CPU 推理 4.3GB 模型；已合成的句子不会重跑 |
