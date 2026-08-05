//! Strongly-typed identifiers used throughout MemMux.
//!
//! Newtypes keep a `TaskId` from being accidentally passed where a `RepositoryId` is
//! expected. They are cheap wrappers over `String` and serialize transparently.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Operating-system process identifier.
///
/// We use the platform `pid_t` width (`i32`) so values map directly onto `libc` and
/// `/proc` without conversion. Negative values are never valid process ids.
pub type Pid = i32;

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap an existing string as this identifier.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrow the underlying string.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// The conventional short prefix for this identifier kind (e.g. `task`).
            pub const PREFIX: &'static str = $prefix;
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({:?})", stringify!($name), self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

string_id!(
    /// Identifies a durable logical task, independent of process residency.
    TaskId,
    "task"
);
string_id!(
    /// Identifies a repository (a Git repo plus its shared services and policies).
    RepositoryId,
    "repo"
);
string_id!(
    /// Identifies one physical launch of a provider for a logical task.
    RuntimeInstanceId,
    "runtime"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_display_transparently() {
        let t = TaskId::new("task_abc");
        assert_eq!(t.to_string(), "task_abc");
        assert_eq!(t.as_str(), "task_abc");
        assert_eq!(TaskId::PREFIX, "task");
    }

    #[test]
    fn ids_serialize_as_bare_strings() {
        let t = TaskId::new("task_abc");
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"task_abc\"");
        let back: TaskId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn distinct_id_types_are_not_interchangeable_but_share_value() {
        // Compile-time distinctness is the point; here we only assert value round-trips.
        let r = RepositoryId::from("repo_1");
        assert_eq!(r.as_str(), "repo_1");
    }
}
