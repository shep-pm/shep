use crate::brain::SANDBOX_DOG;
use crate::runner::SpawnSpec;
use std::path::Path;

/// Wraps `spec` so the adopted sandbox dog at `dog` launches its command
///
/// The dog becomes the program and runs `launch <sheep> -- <old command>`.
/// `SHEP_HOME` is set to `home` unless the sheep's env already sets it. The
/// spec asks for a launch link, a connection to the shepherd that reads only
/// this sheep's table, so the launch needs no access to the control socket.
/// Every other field of `spec` stays.
#[must_use]
pub fn through_dog(mut spec: SpawnSpec, dog: &str, home: &Path, sheep: &str) -> SpawnSpec {
    let mut args = vec![
        "launch".to_string(),
        sheep.to_string(),
        "--".to_string(),
        core::mem::replace(&mut spec.program, dog.to_string()),
    ];
    args.append(&mut spec.args);
    spec.args = args;
    spec.launch_link = Some(SANDBOX_DOG.to_string());
    spec.env
        .entry("SHEP_HOME".to_string())
        .or_insert_with(|| home.to_string_lossy().into_owned());
    spec
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn plain() -> SpawnSpec {
        SpawnSpec {
            name: "web".into(),
            program: "node".into(),
            args: vec!["app.js".into(), "--port".into(), "8080".into()],
            cwd: Some(PathBuf::from("/srv/web")),
            out_file: PathBuf::from("/logs/web-out.log"),
            err_file: PathBuf::from("/logs/web-err.log"),
            log_timestamps: true,
            channel: true,
            stdin: true,
            leader_only_stop: true,
            ..SpawnSpec::default()
        }
    }

    #[test]
    fn the_dog_path_becomes_the_program_and_the_old_command_follows_the_double_dash() {
        let wrapped = through_dog(
            plain(),
            "/opt/dogs/box",
            Path::new("/home/ada/.shep"),
            "web",
        );

        assert_eq!(wrapped.program, "/opt/dogs/box");
        assert_eq!(
            wrapped.args,
            ["launch", "web", "--", "node", "app.js", "--port", "8080"]
        );
        let mut expected = plain();
        expected.program = "/opt/dogs/box".into();
        expected.args = wrapped.args.clone();
        expected.env = wrapped.env.clone();
        expected.launch_link = wrapped.launch_link.clone();
        assert_eq!(wrapped, expected);
    }

    #[test]
    fn a_launched_sheep_gets_a_link_on_fd_4() {
        let wrapped = through_dog(
            plain(),
            "/opt/dogs/box",
            Path::new("/home/ada/.shep"),
            "web",
        );

        assert_eq!(wrapped.name, "web");
        assert_eq!(wrapped.launch_link.as_deref(), Some("sandbox"));
        assert!(!wrapped.dog_link, "the sheep is not itself a dog");
    }

    #[test]
    fn shep_home_is_set_when_the_sheep_sets_none_and_left_when_it_does() {
        let home = Path::new("/home/ada/.shep");

        let unset = through_dog(plain(), "/opt/dogs/box", home, "web");
        assert_eq!(
            unset.env.get("SHEP_HOME").map(String::as_str),
            Some("/home/ada/.shep")
        );

        let mut own = plain();
        own.env.insert("SHEP_HOME".into(), "/elsewhere".into());
        own.env.insert("KEEP".into(), "1".into());
        let set = through_dog(own, "/opt/dogs/box", home, "web");
        assert_eq!(
            set.env.get("SHEP_HOME").map(String::as_str),
            Some("/elsewhere")
        );
        assert_eq!(set.env.get("KEEP").map(String::as_str), Some("1"));
    }
}
