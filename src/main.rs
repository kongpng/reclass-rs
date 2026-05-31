//! The `reclass` application binary.
//!
//! Port of `src/main.cpp`. Parses the CLI, sets up logging, and — when built
//! with the `ui` feature — opens the GPUI window (gpui-component `init` → `Root`
//! → workspace, per `gpui_component_cookbook.md`). Without `ui` it runs as a
//! headless engine entry point (logic only), so `--no-default-features` still
//! builds a `reclass` binary.

use clap::Parser;

/// reclass — a structured-data / struct-layout editor.
#[derive(Parser, Debug)]
#[command(name = "reclass", version, about)]
struct Cli {
    /// Open a `.rcx` project file on launch.
    project: Option<String>,

    /// Attach a binary file as the data source on launch.
    #[arg(long)]
    data: Option<String>,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    if let Some(p) = &cli.project {
        tracing::info!(project = %p, "open project requested");
    }
    if let Some(d) = &cli.data {
        tracing::info!(data = %d, "attach data source requested");
    }

    run(cli);
}

#[cfg(feature = "ui")]
fn run(_cli: Cli) {
    // gpui-component setup pattern (gpui_component_cookbook.md §4, build-verified).
    gpui_platform::application().run(move |cx: &mut gpui::App| {
        gpui_component::init(cx); // REQUIRED before using any component.
        reclass::ui::open_main_window(cx);
    });
}

#[cfg(not(feature = "ui"))]
fn run(_cli: Cli) {
    // Headless engine build: no gpui. The editor engine is available via the
    // library API; there is no window to open.
    tracing::info!("reclass headless build (no `ui` feature) — nothing to display");
}
