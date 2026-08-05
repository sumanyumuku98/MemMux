//! Concrete provider adapters (SUM-70 generic, SUM-71 Claude Code, SUM-72 Codex,
//! SUM-73 Gemini CLI, SUM-74 OpenCode).

use crate::adapter::{LaunchSpec, ProviderAdapter};
use crate::capabilities::{
    ContextCompaction, OutputMode, Permissions, ProviderCapabilities, ResumeFidelity,
    SafePointSignal, SubagentVisibility, ToolVisibility,
};
use memmux_core::Provider;
use memmux_lifecycle::SecretRef;
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
            // Claude Code carries a resumable session id (`claude --resume <id>`).
            resume: ResumeFidelity::Native,
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
    fn resume_command(&self, spec: &LaunchSpec, session_ref: &str) -> Option<PtySpec> {
        Some(pty_spec("claude", &["--resume", session_ref], spec))
    }
    fn secret_refs(&self) -> Vec<SecretRef> {
        vec![SecretRef::env("ANTHROPIC_API_KEY")]
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
    fn secret_refs(&self) -> Vec<SecretRef> {
        vec![SecretRef::env("OPENAI_API_KEY")]
    }
    fn waiting_markers(&self) -> &[&str] {
        &["allow", "(y/n)", "approve", "?"]
    }
}

/// Gemini CLI adapter (SUM-73).
#[derive(Debug, Default)]
pub struct GeminiCliAdapter;

impl ProviderAdapter for GeminiCliAdapter {
    fn provider(&self) -> Provider {
        Provider::GeminiCli
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            // Gemini CLI has no session handle we can drive for a lossless resume, so we resume
            // by reconstructing context rather than claiming a native resume we can't deliver.
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
        pty_spec("gemini", &[], spec)
    }
    fn secret_refs(&self) -> Vec<SecretRef> {
        vec![SecretRef::env("GEMINI_API_KEY")]
    }
    fn waiting_markers(&self) -> &[&str] {
        &["(y/n)", "approve", "allow", "? "]
    }
}

/// OpenCode adapter (SUM-74).
#[derive(Debug, Default)]
pub struct OpenCodeAdapter;

impl ProviderAdapter for OpenCodeAdapter {
    fn provider(&self) -> Provider {
        Provider::OpenCode
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            // OpenCode persists sessions and can reopen one by id (`opencode --session <id>`).
            resume: ResumeFidelity::Native,
            output: OutputMode::TerminalText,
            safe_point: SafePointSignal::Inferred,
            tools: ToolVisibility::ProcessDerived,
            subagents: SubagentVisibility::Unsupported,
            context_compaction: ContextCompaction::RuntimeSummary,
            permissions: Permissions::WrapperEnforced,
        }
    }
    fn command(&self, spec: &LaunchSpec) -> PtySpec {
        pty_spec("opencode", &[], spec)
    }
    fn resume_command(&self, spec: &LaunchSpec, session_ref: &str) -> Option<PtySpec> {
        Some(pty_spec("opencode", &["--session", session_ref], spec))
    }
    fn secret_refs(&self) -> Vec<SecretRef> {
        vec![SecretRef::env("ANTHROPIC_API_KEY")]
    }
    fn waiting_markers(&self) -> &[&str] {
        &["(y/n)", "approve", "allow", "? "]
    }
}

/// Resolve the adapter for a provider.
pub fn adapter_for(provider: Provider) -> Box<dyn ProviderAdapter> {
    match provider {
        Provider::ClaudeCode => Box::new(ClaudeCodeAdapter),
        Provider::Codex => Box::new(CodexAdapter),
        Provider::GeminiCli => Box::new(GeminiCliAdapter),
        Provider::OpenCode => Box::new(OpenCodeAdapter),
        Provider::Generic => Box::new(GenericTerminalAdapter),
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
            ResumeFidelity::Native
        );
        assert_eq!(
            CodexAdapter.capabilities().resume,
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
    fn adapter_for_maps_every_provider_to_its_own_adapter() {
        for p in [
            Provider::ClaudeCode,
            Provider::Codex,
            Provider::GeminiCli,
            Provider::OpenCode,
            Provider::Generic,
        ] {
            assert_eq!(adapter_for(p).provider(), p);
        }
    }

    #[test]
    fn native_resume_adapters_build_a_resume_command() {
        use crate::capabilities::ResumeFidelity;
        let spec = LaunchSpec::in_dir("/wt");

        // Claude: native `--resume <id>`.
        let claude = ClaudeCodeAdapter;
        assert_eq!(claude.capabilities().resume, ResumeFidelity::Native);
        let rc = claude
            .resume_command(&spec, "sess_42")
            .expect("native resume");
        assert_eq!(rc.program, "claude");
        assert_eq!(rc.args, vec!["--resume".to_string(), "sess_42".to_string()]);

        // OpenCode: native `--session <id>`.
        let oc = OpenCodeAdapter;
        assert_eq!(oc.capabilities().resume, ResumeFidelity::Native);
        assert_eq!(
            oc.resume_command(&spec, "s1").unwrap().args,
            vec!["--session".to_string(), "s1".to_string()]
        );
    }

    #[test]
    fn non_native_adapters_have_no_resume_command() {
        let spec = LaunchSpec::in_dir("/wt");
        assert!(CodexAdapter.resume_command(&spec, "x").is_none());
        assert!(GeminiCliAdapter.resume_command(&spec, "x").is_none());
        assert!(GenericTerminalAdapter.resume_command(&spec, "x").is_none());
    }

    #[test]
    fn adapters_declare_their_secret_refs() {
        assert_eq!(ClaudeCodeAdapter.secret_refs()[0].name, "ANTHROPIC_API_KEY");
        assert_eq!(CodexAdapter.secret_refs()[0].name, "OPENAI_API_KEY");
        assert_eq!(GeminiCliAdapter.secret_refs()[0].name, "GEMINI_API_KEY");
        // The generic "run anything" adapter needs no secrets.
        assert!(GenericTerminalAdapter.secret_refs().is_empty());
    }
}
