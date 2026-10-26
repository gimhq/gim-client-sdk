//! `register`, `restore`, `link`, `whoami`, `users search`.

use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};

use gim_base::StateReader;
use gim_common::UserId;
use gim_sdk::recovery_kit::{format_recovery_kit, parse_recovery_kit, RecoveryKitLabels};
use gim_sdk::RecoveryCode;
use serde_json::json;

use crate::error::{CliError, Result};
use crate::output::{Output, RegisterOut, UserOut, UsersOut, WhoamiOut};
use crate::profile::{Ctx, GimClient, KeepArg, Profile};

/// The kit text of `user_id` and `code`, with the English labels.
pub fn recovery_kit(user_id: &str, code: &str) -> String {
    let labels = RecoveryKitLabels {
        title: "gim recovery kit".into(),
        account_id: "Account ID".into(),
        recovery_key: "Recovery key".into(),
        note: "Keep this safe and private. With it you can get your account back on a new \
               device. Anyone who has it can too."
            .into(),
    };
    format_recovery_kit(user_id, code, &labels)
}

/// A new profile's client on `--server` (default the built-in server) and
/// the keep server chosen on the command line; it must not hold a session.
async fn new_client(
    dir: &Path,
    server: Option<&str>,
    keep: Option<&KeepArg>,
) -> Result<(Profile, String, GimClient)> {
    let mut profile = Profile::create(dir)?;
    profile.apply_keep(keep)?;
    let server = profile.server(server);
    let client = profile.client(&server)?;
    if client.restore_session().await? {
        return Err(CliError::usage("this profile already has an account"));
    }
    Ok((profile, server, client))
}

pub async fn register(
    dir: &Path,
    server: Option<&str>,
    keep: Option<&KeepArg>,
    handle: &str,
    display_name: &str,
) -> Result<Output> {
    let (mut profile, server, client) = new_client(dir, server, keep).await?;
    let handle = handle.strip_prefix('@').unwrap_or(handle);
    let recovery_code = client.create_identity(display_name).await?;
    profile.config.server = Some(server.clone());
    profile.config.save(&profile.dir)?;
    client.claim_handle(handle).await?;
    let session = client.session().ok_or_else(CliError::not_registered)?;
    let out = RegisterOut {
        user_id: session.device.user_id.to_string(),
        device_id: session.device.device_id.to_string(),
        handle: handle.to_string(),
        server,
        recovery_code: recovery_code.to_string(),
    };
    let kit = recovery_kit(&out.user_id, &out.recovery_code);
    let human = format!(
        "Your account is ready. Username: @{}\n\n\
         Save this recovery kit now. It is shown only once:\n\n{kit}",
        out.handle
    );
    let mut output = Output::new(&out, human);
    output.json["recovery_kit"] = json!(kit);
    Ok(output)
}

/// Where `restore` reads the recovery kit from.
pub struct RestoreArgs {
    pub kit_file: Option<PathBuf>,
    pub account_id: Option<String>,
    pub recovery_key: Option<String>,
    pub keep_devices: bool,
}

/// Restores the account into a new profile from its recovery kit: the
/// kit text from `--kit-file` (`-`: stdin), or `--account-id` and
/// `--recovery-key`, which also override what the kit holds.
pub async fn restore(
    dir: &Path,
    server: Option<&str>,
    keep: Option<&KeepArg>,
    args: RestoreArgs,
) -> Result<Output> {
    let kit = match &args.kit_file {
        Some(path) if path.as_os_str() == "-" => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            parse_recovery_kit(&text)
        }
        Some(path) => parse_recovery_kit(&std::fs::read_to_string(path)?),
        None => Default::default(),
    };
    let user_id = args
        .account_id
        .or(kit.user_id)
        .ok_or_else(|| CliError::usage("no Account ID: pass --kit-file or --account-id"))?;
    let user_id = UserId::try_from(user_id.trim().to_lowercase())
        .map_err(|e| CliError::usage(format!("invalid Account ID: {e}")))?;
    let code = args
        .recovery_key
        .or(kit.recovery_code)
        .ok_or_else(|| CliError::usage("no recovery key: pass --kit-file or --recovery-key"))?;
    let code: RecoveryCode = gim_sdk::recovery_kit::normalize_recovery_code(&code)
        .parse()
        .map_err(|e| CliError::usage(format!("invalid recovery key: {e}")))?;
    let (mut profile, server, client) = new_client(dir, server, keep).await?;
    client
        .restore_from_recovery(&user_id, &code, !args.keep_devices)
        .await?;
    profile.config.server = Some(server.clone());
    profile.config.save(&profile.dir)?;
    let session = client.session().ok_or_else(CliError::not_registered)?;
    let out = json!({
        "user_id": session.device.user_id.to_string(),
        "device_id": session.device.device_id.to_string(),
        "server": server,
    });
    let human = if args.keep_devices {
        "Your account is back on this device."
    } else {
        "Your account is back on this device. Your other devices were removed."
    };
    Ok(Output::new(
        &out,
        format!("{human}\nAsk your friends to add you to your chats again."),
    ))
}

/// Asks on stdin whether the code matches; `yes` skips the question.
fn confirm_code(code: &str, yes: bool) -> Result<bool> {
    eprintln!("Check that the other device shows this code: {code}");
    if yes {
        return Ok(true);
    }
    eprint!("Does it match? [y/N] ");
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

/// Links this new profile to an account: prints the link code to enter on
/// a device of that account (`gim devices add <code>`), waits for it and
/// compares the security code.
pub async fn link(
    dir: &Path,
    server: Option<&str>,
    keep: Option<&KeepArg>,
    yes: bool,
) -> Result<Output> {
    let (mut profile, server, client) = new_client(dir, server, keep).await?;
    let session = client.start_link(vec![server.clone()])?;
    eprintln!(
        "On a device that has your account, run:\n  gim devices add '{}'",
        session.qr_string
    );
    eprintln!("Waiting...");
    let linked = client.wait_link(&session).await?;
    let ok = confirm_code(&linked.sas.to_string(), yes)?;
    client.confirm_link(ok).await?;
    if !ok {
        return Err(CliError::other(
            "The codes did not match, so this device was not added.",
        ));
    }
    profile.config.server = Some(server.clone());
    profile.config.save(&profile.dir)?;
    let session = client.session().ok_or_else(CliError::not_registered)?;
    let out = json!({
        "user_id": session.device.user_id.to_string(),
        "device_id": session.device.device_id.to_string(),
        "server": server,
    });
    Ok(Output::new(
        &out,
        "This device is now linked to your account.",
    ))
}

pub fn whoami(ctx: &Ctx) -> Result<Output> {
    let device = &ctx.session.device;
    let me = ctx.store().user(&device.user_id)?;
    let out = WhoamiOut {
        user_id: device.user_id.to_string(),
        device_id: device.device_id.to_string(),
        handle: me.as_ref().map(|u| u.handle.clone()),
        display_name: me.map(|u| u.display_name),
        server: ctx.server.clone(),
        keep_server: ctx.keep_server.clone(),
    };
    let human = format!(
        "Account ID: {}\nDevice: {}\nUsername: {}\nName: {}\nServer: {}\nKeep server: {}",
        out.user_id,
        out.device_id,
        out.handle.as_deref().unwrap_or("-"),
        out.display_name.as_deref().unwrap_or("-"),
        out.server,
        out.keep_server.as_deref().unwrap_or("off"),
    );
    Ok(Output::new(&out, human))
}

/// Exact handle lookup: the SDK has no substring search.
pub async fn search_users(ctx: &Ctx, query: &str) -> Result<Output> {
    let query = query.strip_prefix('@').unwrap_or(query);
    let users: Vec<UserOut> = ctx
        .client
        .find_user(query)
        .await?
        .iter()
        .map(UserOut::from)
        .collect();
    let human = users
        .iter()
        .map(|u| format!("@{}\t{}\t{}", u.handle, u.display_name, u.user_id))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Output::new(&UsersOut { users }, human))
}
