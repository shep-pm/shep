//! The dog `tests/pane.rs` boots a real shepherd around: it declares a pane
//! with `Probe::pane`, answers `shep.pane` with `pane_answer`, and writes
//! `$SHEP_HOME/pane.ready` once it is answering.
//!
//! The pane has a fields section, a table, and a row action on the table.

#[cfg(unix)]
mod dog {
    use std::process::ExitCode;

    use shep_client::dogs::{self, Destructive, PANE_ACTION, Pane, Probe, Row, Stop, pane_answer};

    #[shep_client::dogs::dog_config]
    #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
    struct Settings {}

    /// The pane, built afresh on every ask.
    fn build() -> Pane {
        Pane::new()
            .fields("host", [("VRAM", "20 GiB"), ("Driver", "570.1")])
            .table(
                "models",
                ["MODEL", "STATE"],
                [
                    Row::new("qwen3-32b", ["qwen3-32b", "loaded"]),
                    Row::new("llama-3-8b", ["llama-3-8b", "idle"]),
                ],
            )
            .row_action("unload", "Unload", Destructive::Yes)
    }

    #[tokio::main]
    pub async fn run() -> ExitCode {
        Probe::new("pane-dog", "0.1.0")
            .ask_for_channel()
            .pane()
            .answer::<Settings>();

        let paths = dogs::resolve_paths(&|key| std::env::var_os(key)).expect("SHEP_HOME");
        let shepherd = shep_channel::serve();
        shepherd.on_action(PANE_ACTION, |params, _name| pane_answer(params, build));
        shepherd.on_shutdown(|| std::process::exit(0));
        shepherd.ready().expect("say ready");
        std::fs::write(paths.home.join("pane.ready"), "").expect("mark ready");

        Stop::on_stop_signals().wait().await;
        ExitCode::SUCCESS
    }
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    dog::run()
}

#[cfg(not(unix))]
fn main() {}
