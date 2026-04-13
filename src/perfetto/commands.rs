use serde::{Deserialize, Serialize};

/// A single Perfetto UI startup command. Executed sequentially when a trace
/// loads in ui.perfetto.dev.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartupCommand {
    pub id: String,
    pub args: Vec<String>,
}

/// Metadata about one argument a command accepts.
#[derive(Debug, Clone)]
pub struct ArgSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub required: bool,
}

/// Metadata about a known startup command from the Perfetto reference.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CommandSpec {
    pub id: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub args: &'static [ArgSpec],
}

/// Complete catalog of Perfetto UI startup commands.
/// Source: https://perfetto.dev/docs/visualization/commands-automation-reference
#[allow(dead_code)]
pub const COMMAND_CATALOG: &[CommandSpec] = &[
    // --- Track manipulation ---
    CommandSpec {
        id: "dev.perfetto.PinTracksByRegex",
        category: "Tracks",
        description: "Pin tracks matching a regex pattern",
        args: &[
            ArgSpec {
                name: "pattern",
                description: "Regex to match track names or paths",
                required: true,
            },
            ArgSpec {
                name: "nameOrPath",
                description: "Match against 'name' or 'path' (default: name)",
                required: false,
            },
        ],
    },
    CommandSpec {
        id: "dev.perfetto.ExpandTracksByRegex",
        category: "Tracks",
        description: "Expand tracks matching a regex pattern",
        args: &[
            ArgSpec {
                name: "pattern",
                description: "Regex to match track names or paths",
                required: true,
            },
            ArgSpec {
                name: "nameOrPath",
                description: "Match against 'name' or 'path'",
                required: false,
            },
        ],
    },
    CommandSpec {
        id: "dev.perfetto.CollapseTracksByRegex",
        category: "Tracks",
        description: "Collapse tracks matching a regex pattern",
        args: &[
            ArgSpec {
                name: "pattern",
                description: "Regex to match track names or paths",
                required: true,
            },
            ArgSpec {
                name: "nameOrPath",
                description: "Match against 'name' or 'path'",
                required: false,
            },
        ],
    },
    // --- Debug tracks ---
    CommandSpec {
        id: "dev.perfetto.AddDebugSliceTrack",
        category: "Debug",
        description: "Add a debug slice track from a SQL query",
        args: &[
            ArgSpec {
                name: "query",
                description: "SQL query returning slices (ts, dur, name columns)",
                required: true,
            },
            ArgSpec {
                name: "title",
                description: "Display title for the track",
                required: true,
            },
        ],
    },
    CommandSpec {
        id: "dev.perfetto.AddDebugSliceTrackWithPivot",
        category: "Debug",
        description: "Add a debug slice track with pivot column grouping",
        args: &[
            ArgSpec {
                name: "query",
                description: "SQL query returning slices",
                required: true,
            },
            ArgSpec {
                name: "pivotColumn",
                description: "Column name to pivot/group by",
                required: true,
            },
            ArgSpec {
                name: "title",
                description: "Display title for the track",
                required: true,
            },
        ],
    },
    CommandSpec {
        id: "dev.perfetto.AddDebugCounterTrack",
        category: "Debug",
        description: "Add a debug counter track from a SQL query",
        args: &[
            ArgSpec {
                name: "query",
                description: "SQL query returning counters (ts, value columns)",
                required: true,
            },
            ArgSpec {
                name: "title",
                description: "Display title for the track",
                required: true,
            },
        ],
    },
    CommandSpec {
        id: "dev.perfetto.AddDebugCounterTrackWithPivot",
        category: "Debug",
        description: "Add a debug counter track with pivot column grouping",
        args: &[
            ArgSpec {
                name: "query",
                description: "SQL query returning counters",
                required: true,
            },
            ArgSpec {
                name: "pivotColumn",
                description: "Column name to pivot/group by",
                required: true,
            },
            ArgSpec {
                name: "title",
                description: "Display title for the track",
                required: true,
            },
        ],
    },
    // --- Workspace ---
    CommandSpec {
        id: "dev.perfetto.CreateWorkspace",
        category: "Workspace",
        description: "Create a new workspace",
        args: &[ArgSpec {
            name: "title",
            description: "Workspace title",
            required: true,
        }],
    },
    CommandSpec {
        id: "dev.perfetto.SwitchWorkspace",
        category: "Workspace",
        description: "Switch to an existing workspace",
        args: &[ArgSpec {
            name: "title",
            description: "Workspace title to switch to",
            required: true,
        }],
    },
    CommandSpec {
        id: "dev.perfetto.CopyTracksToWorkspaceByRegex",
        category: "Workspace",
        description: "Copy matching tracks to a workspace",
        args: &[
            ArgSpec {
                name: "pattern",
                description: "Regex to match tracks",
                required: true,
            },
            ArgSpec {
                name: "workspaceTitle",
                description: "Target workspace title",
                required: true,
            },
            ArgSpec {
                name: "nameOrPath",
                description: "Match against 'name' or 'path'",
                required: false,
            },
        ],
    },
    CommandSpec {
        id: "dev.perfetto.CopyTracksToWorkspaceByRegexWithAncestors",
        category: "Workspace",
        description: "Copy matching tracks + ancestors to a workspace",
        args: &[
            ArgSpec {
                name: "pattern",
                description: "Regex to match tracks",
                required: true,
            },
            ArgSpec {
                name: "workspaceTitle",
                description: "Target workspace title",
                required: true,
            },
            ArgSpec {
                name: "nameOrPath",
                description: "Match against 'name' or 'path'",
                required: false,
            },
        ],
    },
    // --- Queries ---
    CommandSpec {
        id: "dev.perfetto.RunQuery",
        category: "Query",
        description: "Run a SQL query silently",
        args: &[ArgSpec {
            name: "query",
            description: "SQL query to execute",
            required: true,
        }],
    },
    CommandSpec {
        id: "dev.perfetto.RunQueryAndShowTab",
        category: "Query",
        description: "Run a SQL query and show the results tab",
        args: &[
            ArgSpec {
                name: "query",
                description: "SQL query to execute",
                required: true,
            },
            ArgSpec {
                name: "title",
                description: "Tab title for the results",
                required: false,
            },
        ],
    },
    // --- Annotations ---
    CommandSpec {
        id: "dev.perfetto.AddNoteAtTimestamp",
        category: "Annotation",
        description: "Add a note at a specific timestamp",
        args: &[
            ArgSpec {
                name: "timestamp",
                description: "Timestamp in nanoseconds",
                required: true,
            },
            ArgSpec {
                name: "text",
                description: "Note text",
                required: true,
            },
        ],
    },
];

/// Serialize a list of startup commands to the JSON format ui.perfetto.dev expects.
///
/// Trailing optional args that are blank or carry the default value (e.g.
/// `nameOrPath` = `"name"`) are stripped so the UI falls back to its own
/// defaults rather than receiving an empty/redundant string.
pub fn serialize_commands(commands: &[StartupCommand]) -> String {
    let trimmed: Vec<StartupCommand> = commands
        .iter()
        .map(|cmd| {
            let mut args = cmd.args.clone();
            if let Some(spec) = find_spec(&cmd.id) {
                // Walk from the tail and pop any optional arg whose value is
                // empty or matches its default.
                while args.len() > 0 {
                    let idx = args.len() - 1;
                    let is_optional = spec.args.get(idx).map_or(true, |a| !a.required);
                    if !is_optional {
                        break;
                    }
                    let val = args[idx].trim();
                    if val.is_empty() || is_default_value(spec, idx, val) {
                        args.pop();
                    } else {
                        break;
                    }
                }
            }
            StartupCommand {
                id: cmd.id.clone(),
                args,
            }
        })
        .collect();
    serde_json::to_string(&trimmed).unwrap_or_else(|_| "[]".into())
}

/// Returns `true` when `value` is the implicit default for the arg at `idx`
/// in the given command spec, meaning the UI will behave the same whether or
/// not we send it.
fn is_default_value(spec: &CommandSpec, idx: usize, value: &str) -> bool {
    match spec.args.get(idx) {
        Some(arg) if arg.name == "nameOrPath" => value.eq_ignore_ascii_case("name"),
        _ => false,
    }
}

/// Find the spec for a command ID from the catalog.
#[allow(dead_code)]
pub fn find_spec(id: &str) -> Option<&'static CommandSpec> {
    COMMAND_CATALOG.iter().find(|s| s.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_roundtrips_required_args() {
        let cmds = vec![
            StartupCommand {
                id: "dev.perfetto.PinTracksByRegex".into(),
                args: vec![".*MyApp.*".into()],
            },
            StartupCommand {
                id: "dev.perfetto.RunQuery".into(),
                args: vec!["SELECT count(*) FROM slice".into()],
            },
        ];
        let json = serialize_commands(&cmds);
        let parsed: Vec<StartupCommand> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, cmds);
    }

    #[test]
    fn serialize_keeps_path_when_set() {
        let cmds = vec![StartupCommand {
            id: "dev.perfetto.PinTracksByRegex".into(),
            args: vec![".*".into(), "path".into()],
        }];
        let json = serialize_commands(&cmds);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        let args = parsed[0]["args"].as_array().unwrap();
        assert_eq!(args, &[".*", "path"]);
    }

    #[test]
    fn serialize_strips_blank_name_or_path() {
        let cmds = vec![StartupCommand {
            id: "dev.perfetto.PinTracksByRegex".into(),
            args: vec![".*".into(), "".into()],
        }];
        let json = serialize_commands(&cmds);
        let parsed: Vec<StartupCommand> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0].args, vec![".*"]);
    }

    #[test]
    fn serialize_strips_default_name_value() {
        // "name" is the default for nameOrPath — should be omitted.
        let cmds = vec![StartupCommand {
            id: "dev.perfetto.ExpandTracksByRegex".into(),
            args: vec!["foo".into(), "name".into()],
        }];
        let json = serialize_commands(&cmds);
        let parsed: Vec<StartupCommand> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0].args, vec!["foo"]);
    }

    #[test]
    fn catalog_has_all_commands() {
        assert_eq!(COMMAND_CATALOG.len(), 14);
        assert!(find_spec("dev.perfetto.PinTracksByRegex").is_some());
        assert!(find_spec("dev.perfetto.RunQueryAndShowTab").is_some());
        assert!(find_spec("nonexistent").is_none());
    }
}
