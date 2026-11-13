# 幻灯片组件

与 `cli/html-ppt` 的 non-code 模板共用画框、字号、颜色和版心：**960×540**，padding `35px 45px 29px`，颜色只用墨黑 + 一个信号橙，层次靠字号和 hairline，不靠卡片、阴影、第二强调色。

生成 deck 的方式是搭积木：复制 `skeleton.html`，按下面的顺序把 `components/*.html` 里的 `<section>` 粘进去，替换文案，改页序号，删掉用不到的页。

```text
template/
├── skeleton.html          # 空骨架：内联 base.css + 切页脚本（←→ 翻页、画框居中、幻灯片下方圆点页码）
├── base.css               # 共享样式（skeleton 已内联，改样式时两处同步）
└── components/
    ├── cover.html         封面
    ├── thesis.html        单点论点
    ├── stats.html         数字组（可接图表）
    ├── chart.html         条形图
    ├── rows.html          行列表
    ├── table.html         数据表格
    ├── callout.html       注意框
    ├── compare.html       两栏/三栏对照
    ├── quote.html         引语
    ├── image.html         图文
    ├── steps.html         三步流程
    ├── code.html           代码页
    ├── code-image.html     代码 + 输出图
    └── closing.html       收尾
```

## 共享原语

每个组件都带这四个部分，缺一个都不算完整页：

| 元素 | 用途 | 规则 |
|---|---|---|
| `.top` | 顶栏 | 左：`<span class="sec">序号</span>` + 章节；右：栏目/关键词。两边都不能空 |
| `.rule` | 顶栏下 hairline | 每页必须有 |
| `.kicker` / `h2` / `.dek` | 标题区 | kicker 是栏目，h2 是主张，dek 是一到两句上下文 |
| `.foot` | 页脚 | 左：本页结论；右：`.en` 来源/关键词。两边都不能空 |
| `.hl` | 关键高亮 | 包住本页最关键的数字、结论、术语，用品牌色（信号橙）突出；每页 1–3 处，不满天飞 |
| `.table` | 数据表格 | 表头 8px 等宽 + 墨线下边；行间 hairline，最后一行不带走线（页脚收底）；要选的行加 `tr class="best"`，只用品牌色和字重，不加底色 |
| `.callout` | 注意框 | 左 2px 品牌色线 + 极淡同色底，一段话；一页最多一个，放正文区最后 |

字号（960 画框，截图 2x）：`h1` 48px、`h2` 33px、`h2.big` 44px、`.dek` 12px、正文 12px、`.foot` 10.5px、`.note` 9px、等宽标签 8–9px。正文不小于 11px。

## 组件选择

| 文件 | 用在哪 | 不要用在哪 |
|---|---|---|
| `cover.html` | 第一页，标题 + 导语 + 作者 + 底部数字条 | 目录页、第二张封面 |
| `thesis.html` | 章节开场、转折、单点结论 | 需要列表/数字/图时 |
| `stats.html` | 3–4 个同口径数字，下方接图表填满下半屏 | 没有可核验数据时 |
| `chart.html` | 前后对比、数量级对比的条形图，突出条加 `bar hot` 用品牌色 | 没有数据、或数据口径不一致时 |
| `rows.html` | 4–5 条并列要点 | 超过 5 条（拆页）、纯流程（用 steps） |
| `table.html` | 三到四个方案按同一组维度横向对比 | 只有一条数据（用 stats）、超过 5 行（拆页或改 rows） |
| `callout.html` | 补一句差别/边界/前提，作为整页落点 | 当正文用（塞两段以上）、或一页放两个 |
| `compare.html` | 两种方案、前后变化、得失 | 一边讲优点一边讲缺点 |
| `quote.html` | 幕间、可查证的引语 | 伪造名人名言 |
| `image.html` | 原文确有配图 | 图与论点无关、或一页塞两张 |
| `steps.html` | 三步方法、时间线 | 并列关系（用 rows） |
| `code.html` | 讲 API 形状、调用顺序、实现片段 | 代码只是摆设、或者要贴整段 30 行以上的源码 |
| `code-image.html` | 代码 + 它画出来的东西（渲染结果、截图、输出） | 图和代码没有直接因果关系 |
| `closing.html` | 最后一页，结论 + 边界 | 引入新论据、写“谢谢观看” |

固定页脚：第一页用 `cover`，最后一页用 `closing`，中间 6–12 页从 `thesis / stats / chart / rows / table / callout / compare / quote / image / steps / code / code-image` 里选。整篇 8–14 页。

## 字段速查

- `cover`：`kicker`、`h1`（判断或冲突）、`.dek`（一句导语）、`.meta`（作者）、`.top` 两侧、`.foot` 两侧
- `thesis`：`h2.big`（一句话，最多两行）、`.dek`（为什么成立）
- `stats`：`.stat b`（数字，可用 `<span class="arrow">→</span>`）、`.stat span`（口径/样本量）、`.note`（来源）；一页最多一个数字用 `.signal`
- `rows`：`.row .key`（序号）、`.nm`（主标）、`.ds`（一句解释，≤40 字）
- `table`：`th`（维度名，2–4 列，第一列是被比较对象）、`td`（同一维度的事实）、`tr class="best"`（本页要选的那行）
- `callout`：`b`（引子，如“关键差别：”）、后面一句完整的话；可与 `table` / `rows` 同页组合
- `compare`：`.col h3`（栏标题）、`.big`（数字/短结论）、`p`（同维度事实）、`.note`（限定）
- `quote`：`.quote-text`（原文引语）、`.quote-cn`（翻译/解读）、`.quote-side`（作者 · 标题 · 年份）
- `image`：`.text p`（≤2 段）、`img src="images/img-XX.jpg"`、`figcaption`（内容 + 来源）；正文区自动铺满，图随剩余高度伸缩
- `.big`（compare 的栏结论）直接加 `hl` 类，用品牌色突显
- `steps`：三步，`.n`、`b`（动词开头）、`p`（输入/输出/失败回退）
- `code`：`.code-label`（语言 · 这段代码干什么）、`pre code` 的 `class="language-XXX"`；一页 12–16 行，行号自动加
- `code-image`：同 `code`，右侧 `figure img`（`images/img-XX.png`）+ `figcaption`（条件与来源）；代码 9–12 行
- `closing`：`h2`（结论）、`.dek`（成立条件/边界）、`.sign`（作者/日期）

## 拼装

```bash
cp <skill>/template/skeleton.html <slug>/index.html
```

1. 把选中的组件 `<section>` 粘进 `<main>`，按叙事顺序排列
2. 每页 `data-index` 从 0 连续；`.top` 的 `.sec` 写两位序号；`.foot` 右侧写 `NN / 总页数`
3. 替换所有占位文案；不要保留模板里的“要点名称”“栏目名”
4. 有配图时把 `images/img-XX.jpg` 放到与 `index.html` 同级的 `images/`
5. 截图：`<skill>/tools/html-video-toolbox/target/release/html-video-toolbox screenshot <slug>/index.html -o <slug>/slides`，逐页看有没有溢出、有没有空白页
6. 页数必须等于 `slides/pages.json` 的 `pages` 数量

## 硬规则

- 一页一个主角：数字页不塞列表，列表页不塞图，引语页不加第二个视觉元素
- `.hl` 只包关键词和关键数字（每页 1–3 处），不要整句整段地包；图表突出条用 `bar hot`（前后对比取“后”，单数据取最亮眼一条），一页最多一条 `.hot`
- `.hl` 是唯一的品牌色高亮；`<em>` 在 `h1` 里是模板的弱化灰（`h1 em { color: var(--muted) }`），不是高亮，别拿它当强调，也不要给它上色
- 浏览器预览时幻灯片在页面居中，键盘 ←→（含空格/Home/End）翻页，幻灯片下方有一排圆点指示当前页（可点击跳转），均为 `skeleton.html` 自带，不要删；截图工具自己会隐藏圆点并按 slide 边界裁切
- 只用 `base.css` 里的类，不写新的页面局部样式；需要新组件就先加进 `components/`
- 不画卡片、不加阴影、不加第二强调色；分隔用 hairline，不用色块
- 标题不写“概述”“背景”“总结”，写判断或冲突
- 数字必须有来源；查不到就删掉，不要用“约”“左右”编一个
- 正文不能进入页脚：`.foot` 上方留白由 `.body` 的 `flex:1` 自然产生，不要用 `position:absolute`
- 页脚线是页面唯一的收底 hairline：`.foot` 正上方的内容（图例、最后一行、note）不要再带 `border-top/bottom`，否则会出现两条相近的分割线；图表图例已去线，行列表用 `.row:last-child { border-bottom: none }`
- 代码块只靠等宽字体、hairline 边框和语法着色区分，不画卡片、不加行号底色；`hljs` 从 CDN 加载（`skeleton.html` 自带），离线时退化成纯文本代码块，行号和布局不受影响
- 代码里的 `&` 和 `<` 必须写成 `&amp;` / `&lt;`，行号由 `skeleton.html` 的脚本按行生成，不要手写行号
