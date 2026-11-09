mod asr;
mod check;
mod compose;
mod pages;
mod screenshot;
mod selftest;
mod state;
mod text;
mod tts;
mod verify;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "html-video-toolbox",
    version,
    about = "article-video pipeline: screenshot / sentence mapping / checks / state / tts / compose / verify"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// screenshot HTML slides page by page (headless Chromium)
    Screenshot(screenshot::Args),
    /// milestone state: status / done / reset
    State {
        #[command(subcommand)]
        cmd: state::Command,
    },
    /// document checks: script / speech / tts-text
    Check(check::Args),
    /// script.md + speech.txt → slides/pages.json
    Pages(pages::Args),
    /// per-sentence TTS + silence trim + audio concat + SRT
    Tts(tts::Args),
    /// transcribe voice/*.wav and compare with speech.tts.txt
    Asr(asr::Args),
    /// slides + audio -> final.mp4 without subtitles
    Compose(compose::Args),
    /// final video acceptance
    Verify(verify::Args),
    /// model-free selftest
    Selftest,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Screenshot(args) => screenshot::run(args),
        Command::State { cmd } => state::run(cmd),
        Command::Check(args) => check::run(args),
        Command::Pages(args) => pages::run(args),
        Command::Tts(args) => tts::run(args),
        Command::Asr(args) => asr::run(args),
        Command::Compose(args) => compose::run(args),
        Command::Verify(args) => verify::run(args),
        Command::Selftest => selftest::run(),
    };
    if let Err(err) = result {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}
