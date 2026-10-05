//! `shep answer` against a real shepherd: a sheep asks over its channel and
//! the operator's answer comes back down it.

use super::*;

#[cfg(unix)]
/// The sheep writes its question to fd 3, then records the first line the
/// shepherd sends back to a file the test reads.
#[test]
fn an_operator_answers_a_question_a_sheep_asked_over_its_channel() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let received = dir.path().join("received.txt");
    let script = write_script(
        &dir,
        "asker.sh",
        &format!(
            "#!/bin/sh\n\
             printf '{{\"kind\":\"ask\",\"question\":\"q1\",\"text\":\"Ship it?\",\"takes\":\"yes-no\"}}\\n' >&3\n\
             read -r line <&3\n\
             printf '%s\\n' \"$line\" > '{}'\n\
             sleep 300\n",
            received.display()
        ),
    );
    let flockfile = write_flockfile(
        &dir,
        &format!(
            "[[app]]\nname = \"asker\"\nscript = '{}'\nchannel = true\n",
            script.display()
        ),
    );
    let mut guard = DaemonGuard::default();

    let started = shep(home).arg("start").arg(&flockfile).output().unwrap();
    guard.adopt_home(home);
    assert_success(&started);

    let start = Instant::now();
    let listed = loop {
        let output = shep(home)
            .args(["--format", "json", "answer"])
            .output()
            .unwrap();
        assert_success(&output);
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let rows = envelope["data"].as_array().cloned().unwrap_or_default();
        if rows.iter().any(|row| row["question"] == "q1") || start.elapsed() >= FLOCK_DEADLINE {
            break rows;
        }
        std::thread::sleep(FLOCK_POLL_INTERVAL);
    };
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["name"], "asker");
    assert_eq!(listed[0]["text"], "Ship it?");
    assert_eq!(listed[0]["takes"], "yes-no");

    let answered = shep(home)
        .args(["answer", "asker", "q1", "no", "rebase", "first"])
        .output()
        .unwrap();
    assert_success(&answered);
    assert_eq!(
        String::from_utf8_lossy(&answered.stdout),
        "answered q1 on asker\n"
    );

    let start = Instant::now();
    let line = loop {
        let text = std::fs::read_to_string(&received).unwrap_or_default();
        if text.ends_with('\n') || start.elapsed() >= FLOCK_DEADLINE {
            break text;
        }
        std::thread::sleep(FLOCK_POLL_INTERVAL);
    };
    let delivered: serde_json::Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|err| panic!("the sheep read {line:?}, which is not JSON: {err}"));
    assert_eq!(delivered["answer"], "no", "{delivered}");
    assert_eq!(delivered["note"], "rebase first", "{delivered}");

    let again = shep(home)
        .args(["answer", "asker", "q1", "yes"])
        .output()
        .unwrap();
    assert_eq!(again.status.code(), Some(3), "{again:?}");

    graceful_kill(home);
}
