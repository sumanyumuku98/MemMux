//! Concrete provider adapters (SUM-70 generic, SUM-71 Claude Code, SUM-72 Codex).

use crate::adapter::{LaunchSpec, ProviderAdapter};
use crate::capabilities::{
    ContextCompaction, OutputMode, Permissions, ProviderCapabilities, ResumeFidelity,
    SafePointSignal, SubagentVisibility, ToolVisibility,
};
use memmux_core::Provider;
use memmux_pty::PtySpec;

const DEFAULT_ROWS: u16 = 24;
const DEFAULT_COLS: u16 = 80;

fn pty_spec(program: &str, base_args: &[&str], spec: &LaunchSpec) -> PtySpec {
    let mut args: Vec<String> = base_args.iter().map(|s| s.to_string()).collect();
    args.extend(spec.extra_args.iter().cloned());
    PtySpec {
        program: program.to_string(),
        args,
        cwd: spec.cwd.clone(),
        env: spec.env.clone(),
        rows: if spec.rows == 0 {
            DEFAULT_ROWS
        } else {
            spec.rows
        },
        cols: if spec.cols == 0 {
            DEFAULT_COLS
        } else {
            spec.cols
        },
    }
}

/// Generic "run anything" adapter (SUM-70): runs the exact command in `LaunchSpec::command`.
#[derive(Debug, Default)]
pub struct GenericTerminalAdapter;

impl ProviderAdapter for GenericTerminalAdapter {
    fn provider(&self) -> Provider {
        Provider::Generic
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::conservative()
    }
    fn command(&self, spec: &LaunchSpec) -> PtySpec {
        match spec.command.as_deref() {
            Some([program, rest @ ..]) => {
                let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
                pty_spec(program, &refs, spec)
            }
            _ => pty_spec("sh", &["-c", ":"], spec),
        }
    }
}

/// Claude Code adapter (SUM-71).
#[derive(Debug, Default)]
pub struct ClaudeCodeAdapter;

impl ProviderAdapter for ClaudeCodeAdapter {
    fn provider(&self) -> Provider {
        Provider::ClaudeCode
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            resume: ResumeFidelity::Reconstructed,
            output: OutputMode::Hybrid,
            safe_point: SafePointSignal::Inferred,
            tools: ToolVisibility::ProcessDerived,
            subagents: SubagentVisibility::ProcessDerived,
            context_compaction: ContextCompaction::ProviderCommand,
            permissions: Permissions::Both,
        }
    }
    fn command(&self, spec: &LaunchSpec) -> PtySpec {
        pty_spec("claude", &[], spec)
    }
    fn waiting_markers(&self) -> &[&str] {
        &["Do you want", "(y/n)", "Press enter", "approve", "❯", "? "]
    }
}

/// Codex adapter (SUM-72).
#[derive(Debug, Default)]
pub struct CodexAdapter;

impl ProviderAdapter for CodexAdapter {
    fn provider(&self) -> Provider {
        Provider::Codex
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            resume: ResumeFidelity::Reconstructed,
            output: OutputMode::TerminalText,
            safe_point: SafePointSignal::Inferred,
            tools: ToolVisibility::ProcessDerived,
            subagents: SubagentVisibility::Unsupported,
            context_compaction: ContextCompaction::RuntimeSummary,
            permissions: Permissions::WrapperEnforced,
        }
    }
    fn command(&self, spec: &LaunchSpec) -> PtySpec {
        pty_spec("codex", &[], spec)
    }
    fn waiting_markers(&self) -> &[&str] {
        &["allow", "(y/n)", "approve", "?"]
    }
}

/// Resolve the adapter for a provider.
pub fn adapter_for(provider: Provider) -> Box<dyn ProviderAdapter> {
    match provider {
        Provider::ClaudeCode => Box::new(ClaudeCodeAdapter),
        Provider::Codex => Box::new(CodexAdapter),
        Provider::Generic => Box::new(GenericTerminalAdapter),
        // Gemini CLI and OpenCode adapters arrive in Phase 2 (SUM-73/74); fall back to generic.
        Provider::GeminiCli | Provider::OpenCode => Box::new(GenericTerminalAdapter),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memmux_core::TaskState;

    #[test]
    fn generic_runs_the_given_command() {
        let spec = LaunchSpec {
            command: Some(vec!["echo".into(), "hi".into()]),
            ..LaunchSpec::in_dir("/tmp")
        };
        let pty = GenericTerminalAdapter.command(&spec);
        assert_eq!(pty.program, "echo");
        assert_eq!(pty.args, vec!["hi".to_string()]);
        assert_eq!(pty.rows, 24);
    }

    #[test]
    fn claude_and_codex_declare_provider_and_command() {
        assert_eq!(ClaudeCodeAdapter.provider(), Provider::ClaudeCode);
        assert_eq!(
            ClaudeCodeAdapter.command(&LaunchSpec::default()).program,
            "claude"
        );
        assert_eq!(
            CodexAdapter.command(&LaunchSpec::default()).program,
            "codex"
        );
        // Capabilities are provider-specific and honest.
        assert_eq!(
            ClaudeCodeAdapter.capabilities().resume,
            ResumeFidelity::Reconstructed
        );
        assert_eq!(
            GenericTerminalAdapter.capabilities().resume,
            ResumeFidelity::Unsupported
        );
    }

    #[test]
    fn classify_detects_waiting_prompt() {
        use crate::adapter::EventWindow;
        let window = EventWindow {
            recent_lines: vec![
                "Applying changes".into(),
                "Do you want to proceed? (y/n)".into(),
            ],
            last_activity_ms: 1_000,
            tool_running: false,
        };
        assert_eq!(
            ClaudeCodeAdapter.classify(&window, 1_500, 5_000),
            TaskState::WaitingUser
        );
    }

    #[test]
    fn classify_idle_and_active() {
        use crate::adapter::EventWindow;
        let window = EventWindow {
            recent_lines: vec!["working".into()],
            last_activity_ms: 1_000,
            tool_running: false,
        };
        assert_eq!(
            CodexAdapter.classify(&window, 10_000, 5_000),
            TaskState::Idle
        );
        assert_eq!(
            CodexAdapter.classify(&window, 2_000, 5_000),
            TaskState::Active
        );
    }

    #[test]
    fn adapter_for_maps_providers() {
        assert_eq!(
            adapter_for(Provider::ClaudeCode).provider(),
            Provider::ClaudeCode
        );
        assert_eq!(
            adapter_for(Provider::GeminiCli).provider(),
            Provider::Generic
        );
    }
}
