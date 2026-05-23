use crate::tools::{builtin, Permissions, ShellPermissions};

use super::{AgentKind, AgentProfile, AgentSource, AgentToolFilter};

pub(super) fn profile() -> AgentProfile {
    AgentProfile {
        kind: AgentKind::Plan,
        source: AgentSource::Builtin,
        name: "explore".to_string(),
        subagent: true,
        display_name: Some("Explore".to_string()),
        description: Some("Fast read-only codebase exploration. Use for finding files by patterns, searching code for keywords, or answering code structure questions; specify the desired thoroughness when calling.".to_string()),
        developer_prompt: PROMPT.to_string(),
        tools: AgentToolFilter::builtin_with_extra(vec![
            builtin::BuiltinToolId::ReadFile,
            builtin::BuiltinToolId::ListDir,
            builtin::BuiltinToolId::GrepFiles,
            builtin::BuiltinToolId::ViewImage,
            builtin::BuiltinToolId::WebSearch,
            builtin::BuiltinToolId::ShellCommand,
            builtin::BuiltinToolId::WriteStdin,
            builtin::BuiltinToolId::UpdatePlan,
        ]),
        permissions: permissions(),
        runtime: None,
    }
}

fn permissions() -> Permissions {
    Permissions {
        file_read: true,
        file_write: false,
        shell: ShellPermissions::AllowList(vec![
            vec!["ls".into()],
            vec!["cat".into()],
            vec!["rg".into()],
            vec!["find".into()],
            vec!["sed".into()],
            vec!["head".into()],
            vec!["tail".into()],
            vec!["pwd".into()],
            vec!["git".into(), "status".into()],
            vec!["git".into(), "diff".into()],
        ]),
    }
}

const PROMPT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/explore.md"));
