use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;

use super::Ctx;

/// The agent skill teaching coding agents to drive the headless CLI.
/// `{{version}}` is stamped at install time so stale installs are visible.
const SKILL_MD: &str = include_str!("../../assets/skills/perfetto-cli/SKILL.md");
const SKILL_NAME: &str = "perfetto-cli";

#[derive(Subcommand)]
pub enum SkillsCommand {
    /// Install the perfetto-cli agent skill. Defaults to Claude Code's
    /// user-level skills dir (`~/.claude/skills`), so it works in every
    /// project. Re-run after upgrading to refresh it.
    Install(InstallArgs),
}

#[derive(Args)]
pub struct InstallArgs {
    /// Install into `./.claude/skills` of the current project instead.
    #[arg(long, conflicts_with = "dir")]
    project: bool,
    /// Install into `<DIR>/perfetto-cli/` — for other agents' skills dirs.
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,
}

#[derive(Serialize)]
struct Installed {
    path: String,
    status: Status,
}

#[derive(Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Installed,
    Updated,
    Unchanged,
}

pub fn run_skills(ctx: &Ctx, cmd: SkillsCommand) -> Result<()> {
    match cmd {
        SkillsCommand::Install(args) => install(ctx, args),
    }
}

fn install(ctx: &Ctx, args: InstallArgs) -> Result<()> {
    let skills_dir = match (args.dir, args.project) {
        (Some(dir), _) => dir,
        (None, true) => std::env::current_dir()?.join(".claude").join("skills"),
        (None, false) => directories::BaseDirs::new()
            .context("failed to resolve home directory")?
            .home_dir()
            .join(".claude")
            .join("skills"),
    };
    let path = skills_dir.join(SKILL_NAME).join("SKILL.md");
    let status = write_skill(&path, &render())?;

    let out = Installed {
        path: path.display().to_string(),
        status,
    };
    ctx.emit(&out, || {
        let verb = match out.status {
            Status::Installed => "Installed",
            Status::Updated => "Updated",
            Status::Unchanged => "Already up to date:",
        };
        println!("{verb} {}", out.path);
    })
}

fn render() -> String {
    SKILL_MD.replace("{{version}}", env!("CARGO_PKG_VERSION"))
}

fn write_skill(path: &Path, content: &str) -> Result<Status> {
    let status = match std::fs::read_to_string(path) {
        Ok(existing) if existing == content => return Ok(Status::Unchanged),
        Ok(_) => Status::Updated,
        Err(_) => Status::Installed,
    };
    let parent = path.parent().context("skill path has no parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    std::fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_skill_has_frontmatter_and_version() {
        let skill = render();
        assert!(skill.starts_with("---\nname: perfetto-cli\ndescription: "));
        assert!(skill.contains(env!("CARGO_PKG_VERSION")));
        assert!(!skill.contains("{{version}}"));
    }

    #[test]
    fn write_skill_reports_install_update_unchanged() {
        let dir = std::env::temp_dir().join(format!("perfetto-cli-skill-{}", std::process::id()));
        let path = dir.join(SKILL_NAME).join("SKILL.md");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(write_skill(&path, "a").unwrap() == Status::Installed);
        assert!(write_skill(&path, "a").unwrap() == Status::Unchanged);
        assert!(write_skill(&path, "b").unwrap() == Status::Updated);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
