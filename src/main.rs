//! The `reclass` application binary.
//!
//! Port of `src/main.cpp`. Parses the CLI, sets up logging, and — when built
//! with the `ui` feature — assembles + runs the GPUI application: it initializes
//! gpui-component (`gpui_component::init`), opens the main window (titlebar +
//! docks + document tabs + bespoke editor surface + workspace/scanner panels +
//! dialogs + theme, per `gpui_component_cookbook.md` §4 and `window.rs`), and
//! runs the event loop. A project/`.rcx` (and an optional `--data` binary) given
//! on the command line is opened into the initial tab after the window comes up
//! (the C++ deferred `project_open(path)` after `window.show()`; main.cpp:8774).
//!
//! Without the `ui` feature it runs as a headless engine entry point (logic
//! only, no gpui), so `--no-default-features` still builds a `reclass` binary
//! that parses the CLI and reports what it would open.

use clap::Parser;

/// reclass — a structured-data / struct-layout editor.
///
/// Open a Reclass project (`.rcx` native JSON or `.xml` ReClass-XML) by passing
/// it as the positional argument; attach a binary file as the document's data
/// source with `--data`. With no arguments, launches to the start page.
#[derive(Parser, Debug, Clone, Default)]
#[command(name = "reclass", version, about)]
struct Cli {
    /// Open a project file on launch (`.rcx` native JSON, or `.xml` ReClass-XML).
    project: Option<String>,

    /// Attach a binary file as the document's data source on launch.
    #[arg(long, value_name = "FILE")]
    data: Option<String>,
}

fn main() {
    // Logging: respect `RUST_LOG`; default to `info` so the document-lifecycle
    // tracing (open/load/attach) is visible out of the box.
    init_tracing();

    let cli = Cli::parse();
    log_cli(&cli);
    run(cli);
}

/// Initialize the `tracing` subscriber from the environment (`RUST_LOG`),
/// defaulting to `info`. Uses `try_init` so it never panics if a global
/// subscriber is already installed (e.g. under a test harness).
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

/// Emit the parsed CLI intent at startup (what we will open / attach).
fn log_cli(cli: &Cli) {
    if let Some(p) = &cli.project {
        tracing::info!(project = %p, "open project requested");
    }
    if let Some(d) = &cli.data {
        tracing::info!(data = %d, "attach data source requested");
    }
}

#[cfg(feature = "ui")]
fn run(cli: Cli) {
    let options = cli.into_startup_options();
    // gpui-component setup pattern (gpui_component_cookbook.md §4, build-verified):
    // build the platform application, register the gpui-component icon asset
    // source (so `IconName` SVGs resolve — without it they render as empty
    // boxes), init gpui-component, then open the main window — optionally opening
    // the CLI project once the window is up.
    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx: &mut gpui::App| {
            gpui_component::init(cx); // REQUIRED before using any component.
            reclass::ui::open_main_window_with(cx, options);
        });
}

#[cfg(not(feature = "ui"))]
fn run(cli: Cli) {
    // Headless engine build: no gpui. The editor engine is available via the
    // library API; there is no window to open. Report what the UI build would do.
    let _ = cli;
    tracing::info!("reclass headless build (no `ui` feature) — nothing to display");
}

#[cfg(feature = "ui")]
impl Cli {
    /// Map the parsed CLI into the UI [`StartupOptions`](reclass::ui::StartupOptions)
    /// (the launch state the window assembles from): the positional project path
    /// and the optional `--data` binary, both turned into `PathBuf`s. Kept as a
    /// pure, side-effect-free mapping so it is unit-testable without a display.
    fn into_startup_options(self) -> reclass::ui::StartupOptions {
        reclass::ui::StartupOptions {
            project: self.project.map(std::path::PathBuf::from),
            data: self.data.map(std::path::PathBuf::from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // CLI parsing is pure (no display), so the argument → intent mapping is
    // unit-tested headlessly here. `try_parse_from` lets us assert without a
    // process exit; the binary name is the conventional argv[0].

    #[test]
    fn parses_no_arguments() {
        let cli = Cli::try_parse_from(["reclass"]).expect("no-arg parse");
        assert_eq!(cli.project, None);
        assert_eq!(cli.data, None);
    }

    #[test]
    fn parses_positional_project() {
        let cli = Cli::try_parse_from(["reclass", "demo.rcx"]).expect("project parse");
        assert_eq!(cli.project.as_deref(), Some("demo.rcx"));
        assert_eq!(cli.data, None);
    }

    #[test]
    fn parses_project_and_data() {
        let cli = Cli::try_parse_from(["reclass", "demo.rcx", "--data", "game.bin"])
            .expect("project+data parse");
        assert_eq!(cli.project.as_deref(), Some("demo.rcx"));
        assert_eq!(cli.data.as_deref(), Some("game.bin"));
    }

    #[test]
    fn data_flag_without_project_is_allowed() {
        // --data without a project is valid (attach a source to a fresh doc).
        let cli = Cli::try_parse_from(["reclass", "--data", "game.bin"]).expect("data-only parse");
        assert_eq!(cli.project, None);
        assert_eq!(cli.data.as_deref(), Some("game.bin"));
    }

    #[test]
    fn rejects_unknown_flag() {
        assert!(Cli::try_parse_from(["reclass", "--definitely-not-a-flag"]).is_err());
    }

    #[test]
    fn rejects_extra_positional() {
        // Exactly one positional (the project); a second is an error.
        assert!(Cli::try_parse_from(["reclass", "a.rcx", "b.rcx"]).is_err());
    }

    #[cfg(feature = "ui")]
    #[test]
    fn startup_options_map_paths_from_cli() {
        use std::path::PathBuf;
        let cli = Cli::try_parse_from(["reclass", "proj.rcx", "--data", "data.bin"]).unwrap();
        let opts = cli.into_startup_options();
        assert_eq!(opts.project, Some(PathBuf::from("proj.rcx")));
        assert_eq!(opts.data, Some(PathBuf::from("data.bin")));
    }

    #[cfg(feature = "ui")]
    #[test]
    fn startup_options_default_when_empty() {
        let cli = Cli::try_parse_from(["reclass"]).unwrap();
        let opts = cli.into_startup_options();
        assert_eq!(opts.project, None);
        assert_eq!(opts.data, None);
    }
}
