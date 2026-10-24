use anyhow::Result;
use clap::Parser;

mod app;
mod buffer;

use app::App;

#[derive(Parser)]
#[command(name = "simple-ss")]
#[command(about = "A minimal screenshot tool for Wayland", long_about = None)]
struct Cli {}

fn main() -> Result<()> {
    let _cli = Cli::parse();

    let mut app = App::new()?;
    app.run()?;

    Ok(())
}
