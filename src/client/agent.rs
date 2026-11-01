use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The CLI that owns a conversation. Bare historical session ids mean Claude.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    #[default]
    Claude,
    Codex,
}

impl AgentKind {
    pub fn binary(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn from_session_key(key: &str) -> Self {
        if key.starts_with("codex:") {
            Self::Codex
        } else {
            Self::Claude
        }
    }

    pub fn session_key(self, id: &str) -> String {
        match self {
            Self::Claude => id.to_string(),
            Self::Codex => format!("codex:{id}"),
        }
    }

    pub fn native_id(key: &str) -> &str {
        key.strip_prefix("codex:").unwrap_or(key)
    }

    pub fn resume_args(self, key: &str) -> Vec<String> {
        let flag = match self {
            Self::Claude => "--resume",
            Self::Codex => "resume",
        };
        vec![flag.into(), Self::native_id(key).into()]
    }

    pub fn shell_command(self, args: &[&str]) -> String {
        let mut command = format!("command {}", self.binary());
        for arg in args {
            command.push(' ');
            command.push_str(&super::herdr::shell_quote(arg));
        }
        command
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.binary())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_keys_keep_legacy_claude_ids_and_separate_codex() {
        assert_eq!(AgentKind::Claude.session_key("same"), "same");
        assert_eq!(AgentKind::Codex.session_key("same"), "codex:same");
        assert_eq!(AgentKind::from_session_key("same"), AgentKind::Claude);
        assert_eq!(AgentKind::from_session_key("codex:same"), AgentKind::Codex);
        assert_eq!(AgentKind::Claude.resume_args("same"), ["--resume", "same"]);
        assert_eq!(
            AgentKind::Codex.resume_args("codex:same"),
            ["resume", "same"]
        );
    }

    #[test]
    fn commands_pass_prompt_and_session_as_literal_arguments() {
        for kind in [AgentKind::Claude, AgentKind::Codex] {
            let prompt = "read 'my note'; $(touch /tmp/do-not-create)";
            let command = kind.shell_command(&[prompt]);
            assert!(command.contains("'\\''my note'\\''"));
            assert!(command.starts_with(&format!("command {} '", kind.binary())));
        }
    }
}
