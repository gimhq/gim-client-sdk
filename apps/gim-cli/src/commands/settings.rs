//! `profile`, `devices`, `servers`, `recovery-key`, `privacy`, `block`,
//! `unblock` and the chat settings (`chats mute|pin|archive`).

use std::io::BufRead;
use std::time::{Duration, Instant};

use gim_base::model::HubState;
use gim_base::StateReader;
use gim_common::DeviceId;
use gim_sdk::{ClientError, MUTED_FOREVER};
use serde_json::json;

use crate::commands::account::recovery_kit;
use crate::error::{CliError, Result};
use crate::output::Output;
use crate::profile::Ctx;

/// How long `devices add` waits for the new device to finish linking.
const ADD_DEVICE_WAIT: Duration = Duration::from_secs(120);

fn done(human: impl Into<String>) -> Output {
    Output::new(&json!({}), human)
}

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

pub async fn set_name(ctx: &Ctx, name: &str) -> Result<Output> {
    ctx.client.set_display_name(name).await?;
    Ok(done(format!("Your name is now {name}.")))
}

/// Claims `username`; the server releases the previous one.
pub async fn set_username(ctx: &Ctx, username: &str) -> Result<Output> {
    let username = username.strip_prefix('@').unwrap_or(username);
    ctx.client.claim_handle(username).await?;
    Ok(done(format!("Your username is now @{username}.")))
}

pub async fn remove_username(ctx: &Ctx) -> Result<Output> {
    let username = own_username(ctx)?.ok_or_else(|| CliError::usage("You have no username."))?;
    ctx.client.release_handle(&username).await?;
    Ok(done(format!("@{username} is no longer your username.")))
}

fn own_username(ctx: &Ctx) -> Result<Option<String>> {
    Ok(ctx
        .store()
        .user(&ctx.session.device.user_id)?
        .map(|u| u.handle)
        .filter(|h| !h.is_empty()))
}

pub async fn devices(ctx: &Ctx) -> Result<Output> {
    let devices = ctx.client.devices().await?;
    let out: Vec<_> = devices
        .iter()
        .map(|d| {
            json!({
                "device_id": d.device_id.to_string(),
                "created_at_ms": d.created_at,
                "is_current": d.is_current,
                "revoked": d.revoked,
            })
        })
        .collect();
    let human = devices
        .iter()
        .filter(|d| !d.revoked)
        .map(|d| {
            let this = if d.is_current { "\t(this device)" } else { "" };
            format!("{}{this}", d.device_id)
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Output::new(&json!({ "devices": out }), human))
}

/// Approves a new device that ran `gim link`, after comparing the code.
pub async fn add_device(ctx: &Ctx, link_code: &str, yes: bool) -> Result<Output> {
    let approval = ctx.client.approve_link(link_code).await?;
    eprintln!(
        "Check that the new device shows this code: {}",
        approval.sas
    );
    let ok = yes || {
        eprint!("Does it match? [y/N] ");
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        matches!(answer.trim(), "y" | "Y" | "yes")
    };
    if !ok {
        ctx.client.confirm_approval(&approval, false).await?;
        return Err(CliError::other(
            "The codes did not match, so the device was removed.",
        ));
    }
    // The new device joins the chats once it confirmed on its side and
    // uploaded its KeyPackages.
    let deadline = Instant::now() + ADD_DEVICE_WAIT;
    loop {
        match ctx.client.confirm_approval(&approval, true).await {
            Err(ClientError::InvalidInput(reason))
                if reason.contains("no KeyPackage yet") && Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            result => break result?,
        }
    }
    let device_id = approval.device.device_id.to_string();
    Ok(Output::new(
        &json!({ "device_id": device_id }),
        "The new device is added to your account.",
    ))
}

pub async fn remove_device(ctx: &Ctx, device_id: &str) -> Result<Output> {
    let device_id = DeviceId::new(device_id)
        .map_err(|_| CliError::usage(format!("unknown device {device_id:?}")))?;
    ctx.client.revoke_device(&device_id).await?;
    Ok(done(
        "The device is removed. It can no longer use your account.",
    ))
}

fn server_state(state: HubState) -> &'static str {
    match state {
        HubState::Verified => "connected",
        HubState::Unverified => "not checked",
        HubState::Removed => "removed",
    }
}

pub async fn servers(ctx: &Ctx) -> Result<Output> {
    let hubs = ctx.client.hubs().await?;
    let out: Vec<_> = hubs
        .iter()
        .map(|h| {
            json!({
                "hub_id": h.hub.hub_id,
                "url": h.hub.url,
                "state": format!("{:?}", h.hub.state).to_lowercase(),
                "priority": h.hub.priority,
                "home": h.home,
                "signed_in": h.signed_in,
            })
        })
        .collect();
    let human = hubs
        .iter()
        .filter(|h| h.hub.state != HubState::Removed)
        .map(|h| {
            let main = if h.home { "\t(main server)" } else { "" };
            format!(
                "{}\t{}\t{}{main}",
                h.hub.url.as_deref().unwrap_or("-"),
                server_state(h.hub.state),
                h.hub.hub_id,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Output::new(&json!({ "servers": out }), human))
}

pub async fn add_server(ctx: &Ctx, url: &str) -> Result<Output> {
    let hub = ctx.client.add_hub(url).await?;
    Ok(Output::new(
        &json!({ "hub_id": hub.hub.hub_id }),
        format!("Server added: {url}"),
    ))
}

pub async fn remove_server(ctx: &Ctx, hub_id: &str, remove_groups: bool) -> Result<Output> {
    ctx.client.remove_hub(hub_id, remove_groups).await?;
    Ok(done("Server removed."))
}

/// A new recovery key; the old one stops working.
pub async fn rotate_recovery_key(ctx: &Ctx) -> Result<Output> {
    let rotation = ctx.client.rotate_recovery_code().await?;
    let user_id = ctx.session.device.user_id.to_string();
    let code = rotation.code.to_string();
    let kit = recovery_kit(&user_id, &code);
    let mut human = format!(
        "Your recovery key changed. The old one no longer works.\n\n\
         Save this recovery kit now. It is shown only once:\n\n{kit}"
    );
    if !rotation.unsynced_hubs.is_empty() {
        human.push_str(
            "\nSome of your servers could not be reached. They still accept the old key until \
             you change it again.\n",
        );
    }
    let out = json!({
        "user_id": user_id,
        "recovery_code": code,
        "recovery_kit": kit,
        "unsynced_hub_ids": rotation.unsynced_hubs,
    });
    Ok(Output::new(&out, human))
}

pub async fn privacy(ctx: &Ctx) -> Result<Output> {
    let privacy = ctx.client.privacy().await?;
    let blocked: Vec<String> = privacy.blocked.iter().map(|u| u.to_string()).collect();
    let human = format!(
        "Read receipts: {}\nTyping indicators: {}\nBlocked: {}",
        on_off(privacy.read_receipts),
        on_off(privacy.typing),
        if blocked.is_empty() {
            "nobody".to_string()
        } else {
            blocked.join(", ")
        }
    );
    let out = json!({
        "read_receipts": privacy.read_receipts,
        "typing_indicators": privacy.typing,
        "blocked_user_ids": blocked,
    });
    Ok(Output::new(&out, human))
}

pub async fn set_read_receipts(ctx: &Ctx, on: bool) -> Result<Output> {
    ctx.client.set_read_receipts(on).await?;
    Ok(done(format!("Read receipts are {}.", on_off(on))))
}

pub async fn set_typing(ctx: &Ctx, on: bool) -> Result<Output> {
    ctx.client.set_typing_indicators(on).await?;
    Ok(done(format!("Typing indicators are {}.", on_off(on))))
}

pub async fn block(ctx: &Ctx, user: &str, blocked: bool) -> Result<Output> {
    let user_id = ctx.resolve_user(user).await?;
    if blocked {
        ctx.client.block_user(&user_id).await?;
    } else {
        ctx.client.unblock_user(&user_id).await?;
    }
    let human = if blocked {
        format!("{user} is blocked. You won't get their messages.")
    } else {
        format!("{user} is unblocked.")
    };
    Ok(Output::new(
        &json!({ "user_id": user_id.to_string() }),
        human,
    ))
}

/// `until_ms`: `None` for good, `Some(0)` to unmute.
pub async fn mute(ctx: &Ctx, chat: &str, until_ms: Option<u64>) -> Result<Output> {
    let chat = ctx.resolve_chat(chat)?;
    let until = until_ms.unwrap_or(MUTED_FOREVER);
    ctx.client.set_chat_muted(&chat.group_id, until).await?;
    let human = match until {
        0 => "Notifications are on for this chat.",
        _ => "This chat is muted.",
    };
    Ok(Output::new(
        &json!({ "chat_id": chat.group_id.to_string() }),
        human,
    ))
}

pub async fn pin(ctx: &Ctx, chat: &str, pinned: bool) -> Result<Output> {
    let chat = ctx.resolve_chat(chat)?;
    ctx.client.set_chat_pinned(&chat.group_id, pinned).await?;
    let human = if pinned {
        "Chat pinned."
    } else {
        "Chat unpinned."
    };
    Ok(Output::new(
        &json!({ "chat_id": chat.group_id.to_string() }),
        human,
    ))
}

pub async fn archive(ctx: &Ctx, chat: &str, archived: bool) -> Result<Output> {
    let chat = ctx.resolve_chat(chat)?;
    ctx.client
        .set_chat_archived(&chat.group_id, archived)
        .await?;
    let human = if archived {
        "Chat archived."
    } else {
        "Chat moved out of the archive."
    };
    Ok(Output::new(
        &json!({ "chat_id": chat.group_id.to_string() }),
        human,
    ))
}
