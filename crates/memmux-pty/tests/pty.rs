//! Integration tests for the PTY session against real processes (SUM-49).
//!
//! Unix-only: they rely on `/bin/sh` and `/bin/cat`.

#![cfg(unix)]

use memmux_pty::{CaptureBuffer, CaptureConfig, PtySession, PtySpec, Screen};
use std::time::Duration;

#[test]
fn pty_captures_process_output_into_buffer_and_screen() {
    let spec = PtySpec::command(
        "/bin/sh",
        [
            "-c".to_string(),
            "printf 'hello-pty\\nsecond-line\\n'".to_string(),
        ],
    );
    let mut session = PtySession::spawn(&spec).expect("spawn pty");
    let out = session.read_output_until_exit(Duration::from_secs(5));
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("hello-pty"), "captured output was: {text:?}");
    assert!(
        text.contains("second-line"),
        "captured output was: {text:?}"
    );

    // The captured bytes flow through the bounded pipeline...
    let mut cap = CaptureBuffer::new(CaptureConfig::default());
    cap.ingest(&text, 0);
    cap.flush(0);
    assert!(cap.rendered_rows() >= 2);

    // ...and reconstruct a screen grid.
    let mut screen = Screen::new(24, 80, 100);
    screen.process(&out);
    assert!(screen.contents().contains("hello-pty"));
}

#[test]
fn pty_forwards_stdin_and_resizes() {
    // `cat` echoes stdin back through the PTY.
    let spec = PtySpec::command("/bin/cat", Vec::<String>::new());
    let mut session = PtySession::spawn(&spec).expect("spawn pty");

    session.resize(30, 100).expect("resize");
    assert!(session.is_running());
    session.write_stdin(b"echo-me\n").expect("write stdin");

    // Give the PTY a moment, then drain available output.
    let mut got = String::new();
    for _ in 0..50 {
        while let Some(chunk) = session.try_read_output() {
            got.push_str(&String::from_utf8_lossy(&chunk));
        }
        if got.contains("echo-me") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        got.contains("echo-me"),
        "expected echoed stdin, got: {got:?}"
    );

    session.kill().expect("kill");
}
