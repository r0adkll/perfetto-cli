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
pub fn serialize_commands(commands: &[StartupCommand]) -> String {
    serde_json::to_string(commands).unwrap_or_else(|_| "[]".into())
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
    fn serialize_roundtrips() {
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
    fn catalog_has_all_commands() {
        assert_eq!(COMMAND_CATALOG.len(), 14);
        assert!(find_spec("dev.perfetto.PinTracksByRegex").is_some());
        assert!(find_spec("dev.perfetto.RunQueryAndShowTab").is_some());
        assert!(find_spec("nonexistent").is_none());
    }
}
