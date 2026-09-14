use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;

use super::Database;
use crate::perfetto::commands::StartupCommand;

#[derive(Debug, Clone)]
pub struct SavedCommandSet {
    pub id: i64,
    pub name: String,
    pub commands: Vec<StartupCommand>,
}

/// Concatenate the commands from every selected set in display order.
///
/// Commands are intentionally not deduplicated: two identical commands may
/// be meaningful when they occur at different points in the startup sequence.
pub fn merge_selected_command_sets(
    sets: &[SavedCommandSet],
    selected: &[bool],
) -> Vec<StartupCommand> {
    sets.iter()
        .zip(selected)
        .filter(|(_, is_selected)| **is_selected)
        .flat_map(|(set, _)| set.commands.iter().cloned())
        .collect()
}

/// Recover a set selection when `commands` is exactly the concatenation of
/// saved sets in their current display order. Sessions persist the flattened
/// command list, so this lets the multi-select picker restore its checkmarks.
pub fn command_sets_matching_commands(
    sets: &[SavedCommandSet],
    commands: &[StartupCommand],
) -> Vec<bool> {
    if commands.is_empty() {
        return vec![false; sets.len()];
    }

    fn match_from(
        sets: &[SavedCommandSet],
        commands: &[StartupCommand],
        set_index: usize,
        command_index: usize,
        selected: &mut [bool],
        failed: &mut HashSet<(usize, usize)>,
    ) -> bool {
        if failed.contains(&(set_index, command_index)) {
            return false;
        }
        if set_index == sets.len() {
            return command_index == commands.len();
        }

        let set_commands = &sets[set_index].commands;
        if !set_commands.is_empty()
            && commands[command_index..].starts_with(set_commands)
        {
            selected[set_index] = true;
            if match_from(
                sets,
                commands,
                set_index + 1,
                command_index + set_commands.len(),
                selected,
                failed,
            ) {
                return true;
            }
            selected[set_index] = false;
        }

        if match_from(
            sets,
            commands,
            set_index + 1,
            command_index,
            selected,
            failed,
        ) {
            true
        } else {
            failed.insert((set_index, command_index));
            false
        }
    }

    let mut selected = vec![false; sets.len()];
    let mut failed = HashSet::new();
    if match_from(sets, commands, 0, 0, &mut selected, &mut failed) {
        selected
    } else {
        vec![false; sets.len()]
    }
}

impl Database {
    pub fn create_command_set(
        &self,
        name: &str,
        commands: &[StartupCommand],
    ) -> Result<i64> {
        let conn = self.lock();
        let json = serde_json::to_string(commands)?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO command_sets (name, commands_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![name, json, now, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_command_set(
        &self,
        id: i64,
        commands: &[StartupCommand],
    ) -> Result<()> {
        let conn = self.lock();
        let json = serde_json::to_string(commands)?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE command_sets SET commands_json = ?1, updated_at = ?2 WHERE id = ?3",
            params![json, now, id],
        )?;
        Ok(())
    }

    pub fn list_command_sets(&self) -> Result<Vec<SavedCommandSet>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, commands_json FROM command_sets ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (id, name, json) = r?;
            let commands: Vec<StartupCommand> =
                serde_json::from_str(&json).context("deserialize command set")?;
            out.push(SavedCommandSet { id, name, commands });
        }
        Ok(out)
    }

    pub fn delete_command_set(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM command_sets WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(id: &str) -> StartupCommand {
        StartupCommand {
            id: id.into(),
            args: Vec::new(),
        }
    }

    fn set(id: i64, name: &str, commands: Vec<StartupCommand>) -> SavedCommandSet {
        SavedCommandSet {
            id,
            name: name.into(),
            commands,
        }
    }

    #[test]
    fn merges_selected_sets_in_display_order() {
        let sets = vec![
            set(1, "Tracks", vec![command("pin"), command("expand")]),
            set(2, "Queries", vec![command("query")]),
            set(3, "Notes", vec![command("note")]),
        ];

        assert_eq!(
            merge_selected_command_sets(&sets, &[true, false, true]),
            vec![command("pin"), command("expand"), command("note")]
        );
    }

    #[test]
    fn recovers_selection_from_merged_commands() {
        let sets = vec![
            set(1, "Tracks", vec![command("pin"), command("expand")]),
            set(2, "Queries", vec![command("query")]),
            set(3, "Notes", vec![command("note")]),
        ];
        let commands = vec![command("pin"), command("expand"), command("note")];

        assert_eq!(
            command_sets_matching_commands(&sets, &commands),
            vec![true, false, true]
        );
    }

    #[test]
    fn unmatched_commands_do_not_guess_a_selection() {
        let sets = vec![set(1, "Tracks", vec![command("pin")])];

        assert_eq!(
            command_sets_matching_commands(&sets, &[command("custom")]),
            vec![false]
        );
    }
}
