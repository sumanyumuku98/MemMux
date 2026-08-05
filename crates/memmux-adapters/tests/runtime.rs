//! Integration test: launch a real process through the generic adapter runtime.

#![cfg(unix)]

use memmux_adapters::{GenericTerminalAdapter, LaunchSpec, RuntimeInstance};
use std::time::Duration;

#[test]
fn generic_adapter_launches_and_captures_output() {
    let adapter = GenericTerminalAdapter;
    let spec = LaunchSpec {
        command: Some(vec![
            "sh".into(),
            "-c".into(),
            "printf 'hello-from-adapter\\n'".into(),
        ]),
        ..LaunchSpec::in_dir("/tmp")
    };

    let mut instance = RuntimeInstance::launch(&adapter, &spec, 0).expect("launch");

    // Pump until the process exits.
    for i in 0..200 {
        instance.pump(i);
        if !instance.is_running() {
            instance.pump(i);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let screen = instance.screen_rows().join("\n");
    assert!(
        screen.contains("hello-from-adapter"),
        "screen was: {screen:?}"
    );
    // Capture stays bounded.
    assert!(instance.capture_resident_bytes() < 1024 * 1024);
}
