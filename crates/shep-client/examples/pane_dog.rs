//! The dog `tests/pane.rs` boots a real shepherd around: it declares a pane
//! with `Probe::pane`, answers `shep.pane` with `pane_answer`, and writes
//! `$SHEP_HOME/pane.ready` once it is answering.
//!
//! The pane has a fields section, a table, and a row action on the table;
//! `unload` removes the row it names, so the next `shep.pane` omits it.

#[cfg(unix)]
mod dog {
    use std::process::ExitCode;
    use std::sync::{Arc, Mutex};

    use shep_client::dogs::{self, Destructive, PANE_ACTION, Pane, Probe, Row, Stop, pane_answer};

    #[shep_client::dogs::dog_config]
    #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
    struct Settings {}

    /// The pane, built afresh on every ask from the models still loaded.
    fn build(models: &[String]) -> Pane {
        Pane::new()
            .fields("host", [("VRAM", "20 GiB"), ("Driver", "570.1")])
            .table(
                "models",
                ["MODEL", "STATE"],
                models
                    .iter()
                    .map(|model| Row::new(model, [model.as_str(), "loaded"])),
            )
            .row_action("unload", "Unload", Destructive::Yes)
    }

    /// The row key a row action carries as `{"row": "<key>"}`.
    fn row_of(params: Option<&str>) -> Option<String> {
        let params: serde_json::Value = serde_json::from_str(params?).ok()?;
        params.get("row")?.as_str().map(str::to_owned)
    }

    #[tokio::main]
    pub async fn run() -> ExitCode {
        Probe::new("pane-dog", "0.1.0")
            .ask_for_channel()
            .pane()
            .action("unload", "Unload a model")
            .answer::<Settings>();

        let models = Arc::new(Mutex::new(vec![
            "qwen3-32b".to_owned(),
            "llama-3-8b".to_owned(),
        ]));
        let paths = dogs::resolve_paths(&|key| std::env::var_os(key)).expect("SHEP_HOME");
        let shepherd = shep_channel::serve();
        let shown = Arc::clone(&models);
        shepherd.on_action(PANE_ACTION, move |params, _name| {
            pane_answer(params, || build(&shown.lock().expect("models lock")))
        });
        shepherd.on_action("unload", move |params, _name| {
            let mut models = models.lock().expect("models lock");
            match row_of(params) {
                Some(row) if models.contains(&row) => {
                    models.retain(|model| *model != row);
                    format!("unloaded {row}")
                }
                _ => "no such model".to_owned(),
            }
        });
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
