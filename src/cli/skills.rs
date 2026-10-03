use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use super::Ctx;

/// The agent skill teaching coding agents to drive the headless CLI.
/// `{{version}}` is stamped at install time so stale installs are visible.
const SKILL_MD: &str = include_str!("../../assets/skills/perfetto-cli/SKILL.md");
const SKILL_NAME: &str = "perfetto-cli";

#[derive(Subcommand)]
pub enum SkillsCommand {
    /// Install the perfetto-cli agent skill (Agent Skills `SKILL.md`
    /// format). Defaults to the user-level skills dirs of every supported
    /// agent, so it works in every project. Re-run after upgrading.
    Install(InstallArgs),
}

/// Coding agents that load Agent Skills. Most share `.agents/skills`;
/// Claude Code only reads `.claude/skills`.
#[derive(ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
enum Agent {
    Claude,
    Codex,
    Gemini,
    Cursor,
    Copilot,
}

impl Agent {
    /// Skills root relative to `$HOME` (user scope) or the project root.
    fn skills_root(self) -> &'static str {
        match self {
            Agent::Claude => ".claude/skills",
            Agent::Codex | Agent::Gemini | Agent::Cursor | Agent::Copilot => ".agents/skills",
        }
    }
}

#[derive(Args)]
pub struct InstallArgs {
    /// Agent to install for (repeatable). Defaults to all of them.
    #[arg(long = "agent", value_enum, value_name = "AGENT")]
    agents: Vec<Agent>,
    /// Install into the current project instead of your home directory.
    #[arg(long, conflicts_with = "dir")]
    project: bool,
    /// Install into `<DIR>/perfetto-cli/` — for agents not listed above.
    #[arg(long, value_name = "DIR", conflicts_with = "agents")]
    dir: Option<PathBuf>,
}

#[derive(Serialize)]
struct Installed {
    path: String,
    status: Status,
}

#[derive(Serialize, PartialEq, Debug)]
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
    let roots = match args.dir {
        Some(dir) => vec![dir],
        None => {
            let base = if args.project {
                std::env::current_dir()?
            } else {
                directories::BaseDirs::new()
                    .context("failed to resolve home directory")?
                    .home_dir()
                    .to_path_buf()
            };
            skills_roots(&args.agents)
                .into_iter()
                .map(|root| base.join(root))
                .collect()
        }
    };

    let content = render();
    let installed = roots
        .into_iter()
        .map(|root| {
            let path = root.join(SKILL_NAME).join("SKILL.md");
            let status = write_skill(&path, &content)?;
            Ok(Installed {
                path: path.display().to_string(),
                status,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    ctx.emit(&installed, || {
        for i in &installed {
            let verb = match i.status {
                Status::Installed => "Installed",
                Status::Updated => "Updated",
                Status::Unchanged => "Up to date",
            };
            println!("{verb:<10} {}", i.path);
        }
    })
}

/// Distinct skills roots for `agents` (all agents when empty), in a stable
/// order so output is deterministic.
fn skills_roots(agents: &[Agent]) -> Vec<&'static str> {
    let agents = if agents.is_empty() {
        Agent::value_variants()
    } else {
        agents
    };
    let mut roots: Vec<&'static str> = agents.iter().map(|a| a.skills_root()).collect();
    roots.sort();
    roots.dedup();
    roots
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
    fn skills_roots_dedupe_shared_dirs() {
        assert_eq!(skills_roots(&[]), vec![".agents/skills", ".claude/skills"]);
        assert_eq!(skills_roots(&[Agent::Claude]), vec![".claude/skills"]);
        assert_eq!(
            skills_roots(&[Agent::Codex, Agent::Cursor, Agent::Gemini]),
            vec![".agents/skills"]
        );
    }

    #[test]
    fn write_skill_reports_install_update_unchanged() {
        let dir = std::env::temp_dir().join(format!("perfetto-cli-skill-{}", std::process::id()));
        let path = dir.join(SKILL_NAME).join("SKILL.md");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(write_skill(&path, "a").unwrap(), Status::Installed);
        assert_eq!(write_skill(&path, "a").unwrap(), Status::Unchanged);
        assert_eq!(write_skill(&path, "b").unwrap(), Status::Updated);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
