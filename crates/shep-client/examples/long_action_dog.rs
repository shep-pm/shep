//! The dog `tests/long_action.rs` boots a real shepherd around, in one of
//! two roles chosen by `LONG_ACTION_ROLE`.
//!
//! `callee` answers `build` with an id and publishes the result later.
//! `caller` triggers it once `$SHEP_HOME/go` exists and writes what
//! `trigger_and_wait` returned to `$SHEP_HOME/<dog>.out`.

#[cfg(unix)]
mod dog {
    use std::path::Path;
    use std::process::ExitCode;
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::json;
    use shep_client::dogs::{self, ActionId, DogIdentity, DogRuntime, Stop};

    /// `build`'s params: `<delay_ms>:<label>`, with `early:` ahead of them to
    /// publish the result before the reply leaves the callee.
    fn parse(params: &str) -> (bool, Duration, String) {
        let (early, rest) = match params.strip_prefix("early:") {
            Some(rest) => (true, rest),
            None => (false, params),
        };
        let (delay, label) = rest.split_once(':').unwrap_or(("0", rest));
        let delay = Duration::from_millis(delay.parse().unwrap_or(0));
        (early, delay, label.to_owned())
    }

    pub async fn run() -> ExitCode {
        let env = |key: &str| std::env::var(key).ok();
        let paths = dogs::resolve_paths(&|key| std::env::var_os(key)).expect("SHEP_HOME");
        let identity = DogIdentity::from_env(&env, "long-action");
        let name = identity.section().to_owned();
        let runtime = DogRuntime::start(identity, paths.clone())
            .await
            .expect("the dog connects to its shepherd");
        let home = paths.home.clone();

        match env("LONG_ACTION_ROLE").as_deref() {
            Some("callee") => callee(runtime, &name, &home).await,
            Some("caller") => caller(&runtime, &name, &home).await,
            other => panic!("LONG_ACTION_ROLE is {other:?}"),
        }
        Stop::on_stop_signals().wait().await;
        ExitCode::SUCCESS
    }

    async fn callee(runtime: DogRuntime, name: &str, home: &Path) {
        let runtime = Arc::new(runtime);
        let handle = tokio::runtime::Handle::current();
        let shepherd = shep_channel::serve();
        shepherd.on_action("build", move |params, _name| {
            let (early, delay, label) = parse(params.unwrap_or_default());
            let (id, body) = ActionId::accept();
            let runtime = Arc::clone(&runtime);
            let work = async move {
                tokio::time::sleep(delay).await;
                let result = Ok(json!({ "built": label }));
                runtime.finish_action(&id, result).await.expect("publish");
            };
            if early {
                handle.block_on(work);
            } else {
                handle.spawn(work);
            }
            body
        });
        shepherd.on_shutdown(|| std::process::exit(0));
        shepherd.ready().expect("say ready");
        std::fs::write(home.join(format!("{name}.ready")), "").expect("mark ready");
    }

    async fn caller(runtime: &DogRuntime, name: &str, home: &Path) {
        while !home.join("go").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let env = |key: &str| std::env::var(key).ok();
        let callee = env("LONG_ACTION_CALLEE").expect("LONG_ACTION_CALLEE");
        let params = env("LONG_ACTION_PARAMS");
        let outcome = runtime
            .trigger_and_wait(&callee, "build", params.as_deref(), Duration::from_secs(20))
            .await;
        let line = match outcome {
            Ok(done) => format!("ok {} {}", done.id, done.result),
            Err(error) => format!("err {error}"),
        };
        let out = home.join(format!("{name}.out"));
        std::fs::write(out.with_extension("tmp"), line).expect("write");
        std::fs::rename(out.with_extension("tmp"), out).expect("publish the outcome");
    }
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> std::process::ExitCode {
    dog::run().await
}

#[cfg(not(unix))]
fn main() {}
