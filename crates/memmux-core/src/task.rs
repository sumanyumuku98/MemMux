//! Task specification and the enumerations that describe how a task should run.
//!
//! Mirrors the example task specification in §16.2 and the resource classes in §7.3 of the
//! MemMux V2 specification.

use crate::ids::{RepositoryId, TaskId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// A coding-agent provider MemMux can supervise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    /// Anthropic Claude Code CLI.
    ClaudeCode,
    /// OpenAI Codex CLI.
    Codex,
    /// Google Gemini CLI.
    GeminiCli,
    /// OpenCode CLI.
    OpenCode,
    /// A generic terminal command ("run anything").
    Generic,
}

impl Provider {
    /// Stable, lower-case slug used in paths, metrics labels, and the wire protocol.
    pub fn slug(self) -> &'static str {
        match self {
            Provider::ClaudeCode => "claude-code",
            Provider::Codex => "codex",
            Provider::GeminiCli => "gemini-cli",
            Provider::OpenCode => "opencode",
            Provider::Generic => "generic",
        }
    }
}

/// Static resource class used to bootstrap reservations before per-provider history exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceClass {
    /// Lightweight edits and exploration.
    Small,
    /// Typical single-agent coding session.
    #[default]
    Standard,
    /// Sessions that drive a browser (e.g. Playwright).
    BrowserHeavy,
    /// Sessions dominated by compilation / large builds.
    BuildHeavy,
    /// User-specified custom envelope.
    Custom,
}

/// Scheduling priority. Higher variants outrank lower ones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Priority {
    /// Background / best-effort.
    Low,
    /// Default priority.
    #[default]
    Normal,
    /// Elevated priority.
    High,
    /// Interactive, user-blocking work.
    Urgent,
}

/// How a writable task is isolated from the base checkout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Isolation {
    /// Dedicated Git worktree (default for writers).
    #[default]
    Worktree,
    /// Experimental changed-file overlay over a shared base.
    Overlay,
    /// Read-only inspector attached to another task's worktree.
    ReadOnly,
}

/// A named MCP tool profile (e.g. `repo-read`) resolved by the tool gateway.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct McpProfile(pub String);

/// Tool requirements for a task, used for reservation and lazy activation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolProfile {
    /// Whether the task expects a browser automation stack.
    #[serde(default)]
    pub browser: bool,
    /// Named MCP profile, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_profile: Option<McpProfile>,
}

/// Per-task policy overrides. `None` means "inherit from the policy hierarchy".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPolicies {
    /// Hibernate after this idle duration.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_secs")]
    pub idle_hibernate: Option<Duration>,
    /// Recycle the provider process once its RSS exceeds this many bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recycle_rss_bytes: Option<u64>,
    /// Cap on concurrent test workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_test_workers: Option<u32>,
}

/// The durable description of a unit of work (§6.1 "Task").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpec {
    /// Identifier assigned when the task is created.
    pub id: TaskId,
    /// Owning repository.
    pub repository: RepositoryId,
    /// Absolute path to the repository root on disk.
    pub repository_path: PathBuf,
    /// Human-readable title.
    pub title: String,
    /// Agent provider to launch.
    pub provider: Provider,
    /// Base branch worktrees are cut from.
    pub base_branch: String,
    /// Isolation strategy.
    #[serde(default)]
    pub isolation: Isolation,
    /// Scheduling priority.
    #[serde(default)]
    pub priority: Priority,
    /// Task ids this task depends on (must complete first).
    #[serde(default)]
    pub dependencies: Vec<TaskId>,
    /// Bootstrap resource class.
    #[serde(default)]
    pub resource_class: ResourceClass,
    /// Tool requirements.
    #[serde(default)]
    pub tools: ToolProfile,
    /// Policy overrides.
    #[serde(default)]
    pub policies: TaskPolicies,
}

impl TaskSpec {
    /// Construct a minimal standard-class task with sensible defaults.
    pub fn new(
        id: impl Into<TaskId>,
        repository: impl Into<RepositoryId>,
        repository_path: impl Into<PathBuf>,
        title: impl Into<String>,
        provider: Provider,
        base_branch: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            repository: repository.into(),
            repository_path: repository_path.into(),
            title: title.into(),
            provider,
            base_branch: base_branch.into(),
            isolation: Isolation::default(),
            priority: Priority::default(),
            dependencies: Vec::new(),
            resource_class: ResourceClass::default(),
            tools: ToolProfile::default(),
            policies: TaskPolicies::default(),
        }
    }
}

/// Serde helper: represent an optional `Duration` as whole seconds.
mod opt_secs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(
        value: &Option<Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.map(|d| d.as_secs()).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        let secs = Option::<u64>::deserialize(deserializer)?;
        Ok(secs.map(Duration::from_secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_orders_low_to_urgent() {
        assert!(Priority::Low < Priority::Normal);
        assert!(Priority::Normal < Priority::High);
        assert!(Priority::High < Priority::Urgent);
    }

    #[test]
    fn provider_slugs_are_stable() {
        assert_eq!(Provider::ClaudeCode.slug(), "claude-code");
        assert_eq!(Provider::GeminiCli.slug(), "gemini-cli");
    }

    #[test]
    fn taskspec_defaults_are_standard_worktree() {
        let spec = TaskSpec::new(
            "task_1",
            "repo_1",
            "/src/product",
            "Refactor auth",
            Provider::ClaudeCode,
            "main",
        );
        assert_eq!(spec.isolation, Isolation::Worktree);
        assert_eq!(spec.priority, Priority::Normal);
        assert_eq!(spec.resource_class, ResourceClass::Standard);
        assert!(spec.dependencies.is_empty());
    }

    #[test]
    fn taskspec_round_trips_through_json() {
        let mut spec = TaskSpec::new(
            "task_1",
            "repo_1",
            "/src/product",
            "Refactor auth",
            Provider::ClaudeCode,
            "main",
        );
        spec.priority = Priority::High;
        spec.policies.idle_hibernate = Some(Duration::from_secs(480));
        spec.policies.recycle_rss_bytes = Some(2_684_354_560);
        spec.tools.mcp_profile = Some(McpProfile("repo-read".into()));

        let json = serde_json::to_string(&spec).unwrap();
        let back: TaskSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
        // idle_hibernate is stored as whole seconds.
        assert!(json.contains("\"idle_hibernate\":480"));
    }
}
