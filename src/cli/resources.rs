use anyhow::Result;
use serde::Serialize;

use super::Ctx;
use crate::adb::{self, DeviceState};
use crate::perfetto::commands::StartupCommand;

pub async fn list_devices(ctx: &Ctx) -> Result<()> {
    #[derive(Serialize)]
    struct DeviceView {
        serial: String,
        state: String,
        model: Option<String>,
        nickname: Option<String>,
    }

    let mut views = Vec::new();
    for d in adb::list_live_devices().await? {
        if d.state == DeviceState::Online {
            ctx.db.upsert_device_seen(&d.serial, d.model.as_deref())?;
        }
        views.push(DeviceView {
            nickname: ctx.db.get_device_nickname(&d.serial)?,
            state: match &d.state {
                DeviceState::Online => "online".into(),
                DeviceState::Offline => "offline".into(),
                DeviceState::Unauthorized => "unauthorized".into(),
                DeviceState::Other(s) => s.clone(),
            },
            serial: d.serial,
            model: d.model,
        });
    }
    ctx.emit(&views, || {
        if views.is_empty() {
            println!("No devices.");
        }
        for v in &views {
            println!(
                "{:<24} {:<12} {}{}",
                v.serial,
                v.state,
                v.model.as_deref().unwrap_or("-"),
                v.nickname
                    .as_deref()
                    .map(|n| format!("  ({n})"))
                    .unwrap_or_default(),
            );
        }
    })
}

pub fn list_configs(ctx: &Ctx) -> Result<()> {
    #[derive(Serialize)]
    struct ConfigView {
        id: i64,
        name: String,
        duration_ms: u32,
        cold_start: bool,
    }

    let views: Vec<ConfigView> = ctx
        .db
        .list_configs()?
        .into_iter()
        .map(|c| ConfigView {
            id: c.id,
            name: c.name,
            duration_ms: c.config.duration_ms,
            cold_start: c.config.cold_start,
        })
        .collect();
    ctx.emit(&views, || {
        if views.is_empty() {
            println!("No saved configs.");
        }
        for v in &views {
            println!(
                "#{:<4} {}  ({:.1}s, {})",
                v.id,
                v.name,
                v.duration_ms as f64 / 1000.0,
                if v.cold_start { "cold start" } else { "warm" },
            );
        }
    })
}

pub fn list_command_sets(ctx: &Ctx) -> Result<()> {
    #[derive(Serialize)]
    struct CommandSetView {
        id: i64,
        name: String,
        commands: Vec<StartupCommand>,
    }

    let views: Vec<CommandSetView> = ctx
        .db
        .list_command_sets()?
        .into_iter()
        .map(|c| CommandSetView {
            id: c.id,
            name: c.name,
            commands: c.commands,
        })
        .collect();
    ctx.emit(&views, || {
        if views.is_empty() {
            println!("No startup command sets.");
        }
        for v in &views {
            println!("#{:<4} {}", v.id, v.name);
            for c in &v.commands {
                println!("       {} {}", c.id, c.args.join(" "));
            }
        }
    })
}
