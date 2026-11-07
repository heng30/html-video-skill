# html-video-toolbox 编码规则

## 语言要求
- 代码中的普通注释（`//` 与非 clap 的 `///`）使用中文。
- clap 结构体/枚举变体上的 `///` 会进入 `--help` 输出，必须写英文。
- 注释以外的中文字符串一律使用英文：报错信息、日志输出、JSON 输出。
- 用户可见输出（clap help、stderr、JSON 字段值）必须全英文，保持简洁。

## 运行
- 二进制：`target/release/html-video-toolbox`（`make release` 构建）。
- 本机 Chrome 未装在标准路径时，用 `--chrome-path` 或 `CHROME_PATH` 指定 Chromium。
