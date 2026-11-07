use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use chromiumoxide::{
    Page,
    browser::{Browser, BrowserConfig},
    cdp::browser_protocol::{
        emulation::SetDeviceMetricsOverrideParams,
        page::{CaptureScreenshotFormat, CaptureScreenshotParams, Viewport},
    },
};
use futures::StreamExt;
use std::path::{Path, PathBuf};

const VIEWPORT_WIDTH: i64 = 1920;
const VIEWPORT_HEIGHT: i64 = 3840;

#[derive(clap::Args)]
pub struct Args {
    /// HTML slide file
    pub html: PathBuf,
    /// output directory
    #[arg(short, long, default_value = ".")]
    pub output: PathBuf,
    /// resolution tier (720/1080/1440/2160): long edge px = tier x 16 / 9
    #[arg(short, long, default_value_t = 1080)]
    pub resolution: u32,
    /// page range, e.g. "1-3", "2,5,7", "1-3,8,10", 1-based
    #[arg(short = 'n', long, default_value = "")]
    pub range: String,
    /// filename prefix: outputs {prefix}{n:02}.png
    #[arg(short, long, default_value = "slide-")]
    pub prefix: String,
    /// Chromium/Chrome executable
    #[arg(long, env = "CHROME_PATH")]
    pub chrome_path: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    if !args.html.exists() {
        bail!("HTML file not found: {}", args.html.display());
    }
    let runtime = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    runtime.block_on(run_async(args))
}

struct TempProfile(PathBuf);

impl TempProfile {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Self(std::env::temp_dir().join(format!(
            "html-video-screenshot-{}-{nanos}",
            std::process::id()
        )))
    }
}

impl Drop for TempProfile {
    fn drop(&mut self) {
        // Chromium 退出时会重建 profile 目录，删到它不再出现为止。
        for _ in 0..40 {
            if !self.0.exists() {
                return;
            }
            let _ = std::fs::remove_dir_all(&self.0);
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn parse_range(range: &str, total: usize) -> Result<Vec<usize>> {
    if range.trim().is_empty() {
        return Ok((0..total).collect());
    }
    let mut indexes = Vec::new();
    for part in range.split(',') {
        let part = part.trim();
        if part.contains('-') {
            let (start, end) = part.split_once('-').unwrap();
            let start: usize = start
                .trim()
                .parse()
                .context(format!("invalid range start: {part}"))?;
            let end: usize = end
                .trim()
                .parse()
                .context(format!("invalid range end: {part}"))?;
            for index in start..=end {
                if index >= 1 && index <= total {
                    indexes.push(index - 1);
                }
            }
        } else {
            let index: usize = part
                .parse()
                .context(format!("invalid page number: {part}"))?;
            if index >= 1 && index <= total {
                indexes.push(index - 1);
            }
        }
    }
    indexes.sort_unstable();
    indexes.dedup();
    Ok(indexes)
}

fn find_chrome(cli: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = cli {
        if !path.is_file() {
            bail!("Chromium not found: {}", path.display());
        }
        return Ok(path);
    }
    let candidates = [
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/brave",
        "/usr/bin/brave-browser",
        "/opt/brave.com/brave/brave-browser",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ];
    if let Some(path) = candidates.iter().map(PathBuf::from).find(|p| p.is_file()) {
        return Ok(path);
    }
    for dir in ["/nix/store", "/snap/bin"] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains("chrom") {
                let path = if dir == "/snap/bin" {
                    entry.path()
                } else {
                    entry.path().join("chrome-linux/chrome")
                };
                if path.is_file() {
                    return Ok(path);
                }
            }
        }
    }
    bail!("no Chromium/Chrome found, set --chrome-path or CHROME_PATH")
}

const HIDE_CHROME_JS: &str = r#"(() => {
    let tag = document.getElementById('__screenshot_style');
    if (!tag) {
        tag = document.createElement('style');
        tag.id = '__screenshot_style';
        document.head.appendChild(tag);
    }
    tag.textContent = `
        .navigation-bar, .dot-indicators, .slide-number { display: none !important; }
        body { padding: 0 !important; margin: 0 !important; min-height: 0 !important; }
        .presentation-container, .presentation { margin: 0 !important; padding: 0 !important; }
    `;
})()"#;

fn activate_js(index: usize) -> String {
    format!(
        "(() => {{
            const slides = document.querySelectorAll('.slide');
            slides.forEach(slide => slide.classList.remove('active'));
            if (slides[{index}]) slides[{index}].classList.add('active');
            return slides.length;
        }})()"
    )
}

const BOUNDS_JS: &str = r#"(() => {
    const slide = document.querySelector('.slide.active');
    if (!slide) return JSON.stringify({x:0,y:0,w:0,h:0});
    const r = slide.getBoundingClientRect();
    return JSON.stringify({x:r.x, y:r.y, w:r.width, h:r.height});
})()"#;

async fn activate(page: &Page, index: usize) -> Result<()> {
    page.evaluate_expression(activate_js(index))
        .await
        .context("failed to activate slide")?;
    page.evaluate_expression(HIDE_CHROME_JS)
        .await
        .context("failed to inject screenshot style")?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    Ok(())
}

/// 截图单页，返回 PNG 字节。
async fn capture(page: &Page, index: usize, resolution: u32) -> Result<Vec<u8>> {
    activate(page, index).await?;
    let bounds: String = page
        .evaluate_expression(BOUNDS_JS)
        .await?
        .into_value()
        .context("failed to read slide bounds")?;
    let value: serde_json::Value = serde_json::from_str(&bounds)?;
    let (x, y, w, h) = (
        value["x"].as_f64().unwrap_or(0.0),
        value["y"].as_f64().unwrap_or(0.0),
        value["w"].as_f64().unwrap_or(0.0),
        value["h"].as_f64().unwrap_or(0.0),
    );
    if w <= 0.0 || h <= 0.0 {
        bail!(
            "slide {} has zero size; check that .slide.active exists",
            index + 1
        );
    }

    let target_long_edge = (resolution as f64 * 16.0 / 9.0).round();
    let scale = target_long_edge / w.max(h);
    page.execute(SetDeviceMetricsOverrideParams::new(
        VIEWPORT_WIDTH,
        VIEWPORT_HEIGHT,
        scale,
        false,
    ))
    .await?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let clip = Viewport::builder()
        .x(x)
        .y(y)
        .width(w)
        .height(h)
        .scale(1.0)
        .build()
        .map_err(|e| anyhow!("invalid clip region: {e}"))?;
    let params = CaptureScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Png)
        .clip(clip)
        .capture_beyond_viewport(true)
        .build();
    let result = page.execute(params).await?;
    let data: &str = result.result.data.as_ref();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .context("failed to decode screenshot data")?;

    page.execute(SetDeviceMetricsOverrideParams::new(
        VIEWPORT_WIDTH,
        VIEWPORT_HEIGHT,
        1.0,
        false,
    ))
    .await?;
    Ok(bytes)
}

async fn run_async(args: Args) -> Result<()> {
    let html = args.html.canonicalize()?;
    let chrome = find_chrome(args.chrome_path.clone())?;
    std::fs::create_dir_all(&args.output)?;
    let profile = TempProfile::new();

    let config = BrowserConfig::builder()
        .chrome_executable(chrome)
        .user_data_dir(&profile.0)
        .arg("--no-sandbox")
        .arg("--disable-gpu")
        .arg("--disable-dev-shm-usage")
        .arg("--hide-scrollbars")
        .window_size(VIEWPORT_WIDTH as u32, VIEWPORT_HEIGHT as u32)
        .build()
        .map_err(|e| anyhow!("invalid Chromium config: {e}"))?;
    let (mut browser, mut handler) = Browser::launch(config)
        .await
        .context("failed to launch Chromium (set --chrome-path)")?;
    let handler_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let page = browser.new_page("about:blank").await?;
    page.execute(SetDeviceMetricsOverrideParams::new(
        VIEWPORT_WIDTH,
        VIEWPORT_HEIGHT,
        1.0,
        false,
    ))
    .await?;
    page.goto(format!("file://{}", html.display())).await?;
    // 等字体与高亮脚本落位
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let total: String = page
        .evaluate_expression("String(document.querySelectorAll('.slide').length)")
        .await?
        .into_value()
        .unwrap_or_else(|_| "0".to_string());
    let total: usize = total
        .trim()
        .parse()
        .context("failed to count .slide elements")?;
    if total == 0 {
        bail!("no .slide in {}", html.display());
    }
    let indexes = parse_range(&args.range, total)?;
    if indexes.is_empty() {
        bail!("no pages in the selected range");
    }

    let mut saved = Vec::new();
    for index in &indexes {
        let bytes = capture(&page, *index, args.resolution).await?;
        let name = format!("{}{:02}.png", args.prefix, index + 1);
        let path = args.output.join(&name);
        std::fs::write(&path, &bytes)
            .with_context(|| format!("cannot write {}", path.display()))?;
        saved.push(json_entry(&path));
        eprintln!("saved {}", path.display());
    }

    browser.close().await?;
    let _ = handler_task.await;

    crate::text::print_json(&serde_json::json!({
        "ok": true,
        "pages": total,
        "exported": indexes.len(),
        "files": saved,
    }));
    Ok(())
}

fn json_entry(path: &Path) -> String {
    path.display().to_string()
}
