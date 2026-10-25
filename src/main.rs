use anyhow::Result;
use clap::Parser;

mod app;
mod cursor;
mod render;

use app::App;

#[derive(Parser)]
#[command(name = "freeze")]
#[command(about = "A minimal screenshot tool for Wayland", long_about = None)]
struct Cli {}

fn main() -> Result<()> {
    let _cli = Cli::parse();

    let mut app = App::default();
    app.run()?;

    Ok(())
}
