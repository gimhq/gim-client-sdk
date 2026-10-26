//! `gim`: command-line client of gim, built on the public gim-sdk API.
//!
//! Every command is one-shot: it opens the profile directory (SQLite store,
//! `secrets.json`, `config.json`), restores the session, catches up with
//! one `sync_once`, runs, and flushes the outbox before exiting. Only
//! `listen` and `wait-for` run the background sync loop, stopped before
//! exiting.
//!
//! A profile must never be used by two `gim` processes at the same time
//! (nor by anything else): there is no locking beyond SQLite's own, and
//! concurrent syncs or sends of one device would corrupt its MLS state.

#![forbid(unsafe_code)]

mod commands;
mod error;
mod output;
mod profile;
mod resolve;
mod secrets;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use crate::commands::{account, chats, listen, media, messages, settings, wait_for};
use crate::error::{exit_codes_help, CliError, Result, EXIT_OK, EXIT_USAGE};
use crate::output::{print_error, Output};
use crate::profile::{Ctx, KeepArg};

#[derive(Debug, Parser)]
#[command(
    name = "gim",
    version,
    about = "Command-line gim client",
    long_about = long_about(),
)]
struct Cli {
    /// Profile directory (store, session, config). Required.
    #[arg(long, env = "GIM_PROFILE", global = true, value_name = "DIR")]
    profile: Option<PathBuf>,
    /// Server base URL; saved to the profile by `register`, `restore` and
    /// `link`, read from it otherwise. Default: https://hub.g.im.
    #[arg(long, env = "GIM_SERVER", global = true, value_name = "URL")]
    server: Option<String>,
    /// Keep server base URL (encrypted backup of the privacy settings,
    /// reports). Saved to the profile whenever given; "default" goes back
    /// to the built-in https://keep.g.im, which is used when none is set.
    #[arg(long, env = "GIM_KEEP_SERVER", global = true, value_name = "URL")]
    keep_server: Option<String>,
    /// Turn the keep server off for this profile (saved; wins over
    /// --keep-server). Everything else works without it; `report` does not.
    #[arg(long, global = true)]
    no_keep: bool,
    /// Print exactly one JSON object on stdout (errors included).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

fn long_about() -> String {
    format!(
        "Command-line gim client. Each command restores the profile's session, \
         syncs once, runs and sends what it queued before exiting. Never use one \
         profile from two processes at once.\n\n{}",
        exit_codes_help()
    )
}

const MEMBERS_ABOUT: &str = "Lists devices known from chat membership only.";

const TYPING_ABOUT: &str = "Typing indicators expire on their own; --stop only takes effect \
     within the same process that sent the start (gim-sdk keeps typing state in memory), \
     so from a fresh process it is a no-op.";

#[derive(Debug, Subcommand)]
enum Command {
    /// Create an account on --server (default https://hub.g.im) with this
    /// device and a username;
    /// prints the recovery kit (Account ID and recovery key) once.
    Register {
        /// Username.
        handle: String,
        display_name: String,
    },
    /// Bring an account back on this new profile from its recovery kit
    /// (on --server). Your other devices are removed unless
    /// --keep-devices.
    Restore {
        /// File with the recovery kit text, in any language; `-` reads it
        /// from stdin.
        #[arg(long, value_name = "FILE")]
        kit_file: Option<PathBuf>,
        /// Account ID (instead of, or overriding, the kit).
        #[arg(long, value_name = "ID")]
        account_id: Option<String>,
        /// Recovery key (instead of, or overriding, the kit).
        #[arg(long, value_name = "KEY")]
        recovery_key: Option<String>,
        /// Keep the account's other devices.
        #[arg(long)]
        keep_devices: bool,
    },
    /// Add this new profile as a device of an account (on --server):
    /// prints a link code for `gim devices add` on a device of the account.
    Link {
        /// Do not ask whether the security codes match.
        #[arg(long)]
        yes: bool,
    },
    /// Show this profile's account and device.
    Whoami,
    /// Your name and username.
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// The devices of your account.
    #[command(subcommand)]
    Devices(DevicesCommand),
    /// The servers your account uses.
    #[command(subcommand)]
    Servers(ServersCommand),
    /// Your recovery key.
    #[command(subcommand, name = "recovery-key")]
    RecoveryKey(RecoveryKeyCommand),
    /// Read receipts, typing indicators and blocked users.
    #[command(subcommand)]
    Privacy(PrivacyCommand),
    /// Block a user (username or Account ID): their messages and invites
    /// are dropped.
    Block { user: String },
    /// Unblock a user.
    Unblock { user: String },
    /// Report a user (username or Account ID) to the keep server's
    /// moderators. The messages you name are sent in plaintext: the server
    /// cannot read your chats. Does not block the user.
    Report {
        user: String,
        #[arg(long, value_enum)]
        reason: Reason,
        /// What happened, in your words (up to 1000 characters).
        #[arg(long)]
        note: Option<String>,
        /// A message to include (its id, from `history`); repeatable.
        #[arg(long = "message", value_name = "MESSAGE_ID")]
        messages: Vec<String>,
    },
    /// Look up users.
    #[command(subcommand)]
    Users(UsersCommand),
    /// List chats.
    #[command(subcommand)]
    Chats(ChatsCommand),
    /// Open (or create) the direct chat with a user (handle, or user id for
    /// users of other hubs); prints the chat id.
    Dm { handle: String },
    /// Manage group chats.
    #[command(subcommand)]
    Group(GroupCommand),
    /// List the members of a chat.
    #[command(long_about = MEMBERS_ABOUT)]
    Members { chat: String },
    /// Send a text message.
    Send {
        chat: String,
        text: String,
        /// Message to reply to.
        #[arg(long, value_name = "MSG")]
        reply: Option<String>,
    },
    /// Send an image.
    SendImage {
        chat: String,
        path: PathBuf,
        #[arg(long)]
        caption: Option<String>,
        /// Message to reply to.
        #[arg(long, value_name = "MSG")]
        reply: Option<String>,
    },
    /// Send a file.
    SendFile {
        chat: String,
        path: PathBuf,
        #[arg(long)]
        caption: Option<String>,
        /// Message to reply to.
        #[arg(long, value_name = "MSG")]
        reply: Option<String>,
    },
    /// React to a message.
    React {
        msg: String,
        emoji: String,
        /// Remove the reaction instead.
        #[arg(long)]
        remove: bool,
    },
    /// Edit an own message.
    Edit { msg: String, text: String },
    /// Delete an own message.
    Delete { msg: String },
    /// Mark a chat read.
    Read { chat: String },
    /// Show a chat's latest messages, oldest first.
    History {
        chat: String,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Send a typing notification.
    #[command(long_about = TYPING_ABOUT)]
    Typing {
        chat: String,
        /// Stop typing instead.
        #[arg(long)]
        stop: bool,
    },
    /// Download the media of a message.
    Download { msg: String, path: PathBuf },
    /// Print events as they arrive until Ctrl-C, --timeout, --count or
    /// --until.
    #[command(after_help = listen::HELP)]
    Listen(listen::ListenArgs),
    /// Wait until a message matches filters, or a message reaches states.
    #[command(after_help = wait_for::HELP)]
    WaitFor(wait_for::WaitForArgs),
}

#[derive(Debug, Subcommand)]
enum UsersCommand {
    /// Find a user by exact handle (0 or 1 result).
    ///
    /// Exact handle lookup; substring search is not available yet.
    Search {
        /// Handle, with or without a leading '@'.
        query: String,
    },
}

#[derive(Debug, Subcommand)]
enum ChatsCommand {
    /// List all chats, most recent first.
    List,
    /// Turn off notifications for a chat, for good or until a time.
    Mute {
        chat: String,
        /// Until this Unix time in milliseconds.
        #[arg(long, value_name = "MS")]
        until_ms: Option<u64>,
    },
    /// Turn notifications for a chat back on.
    Unmute {
        chat: String,
    },
    /// Keep a chat at the top of the list.
    Pin {
        chat: String,
    },
    Unpin {
        chat: String,
    },
    /// Move a chat to the archive.
    Archive {
        chat: String,
    },
    /// Move a chat out of the archive.
    Unarchive {
        chat: String,
    },
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    /// Show your account (like `whoami`).
    Show,
    /// Change your name.
    Name { name: String },
    /// Your username.
    #[command(subcommand)]
    Username(UsernameCommand),
}

#[derive(Debug, Subcommand)]
enum UsernameCommand {
    /// Pick a username, or change it.
    Set { username: String },
    /// Remove your username.
    Remove,
}

#[derive(Debug, Subcommand)]
enum DevicesCommand {
    /// List your devices.
    List,
    /// Add a new device: the link code is what `gim link` printed there.
    Add {
        link_code: String,
        /// Do not ask whether the security codes match.
        #[arg(long)]
        yes: bool,
    },
    /// Remove a device: it can no longer use your account.
    Remove { device_id: String },
}

#[derive(Debug, Subcommand)]
enum ServersCommand {
    /// List your servers.
    List,
    /// Add a server by its address.
    Add { url: String },
    /// Remove a server (its id, from `servers list`).
    Remove {
        server_id: String,
        /// Also when group chats live on it (their members lose them).
        #[arg(long)]
        remove_groups: bool,
    },
}

#[derive(Debug, Subcommand)]
enum RecoveryKeyCommand {
    /// Make a new recovery key and print the new recovery kit; the old key
    /// stops working.
    Change,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Reason {
    Spam,
    Harassment,
    Inappropriate,
    Scam,
    Other,
}

impl From<Reason> for gim_sdk::ReportReason {
    fn from(reason: Reason) -> Self {
        match reason {
            Reason::Spam => Self::Spam,
            Reason::Harassment => Self::Harassment,
            Reason::Inappropriate => Self::Inappropriate,
            Reason::Scam => Self::Scam,
            Reason::Other => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Switch {
    On,
    Off,
}

#[derive(Debug, Subcommand)]
enum PrivacyCommand {
    /// Show the privacy settings.
    Show,
    /// Send read receipts (and see those of others).
    ReadReceipts { state: Switch },
    /// Send typing indicators (and see those of others).
    Typing { state: Switch },
}

#[derive(Debug, Subcommand)]
enum GroupCommand {
    /// Create a group chat; prints the chat id.
    Create {
        name: String,
        #[arg(required = true)]
        handles: Vec<String>,
    },
    /// Add users to a group.
    Add {
        chat: String,
        #[arg(required = true)]
        handles: Vec<String>,
    },
    /// Remove users from a group.
    Remove {
        chat: String,
        #[arg(required = true)]
        handles: Vec<String>,
    },
    /// Rename a group.
    Rename { chat: String, name: String },
    /// Leave a group.
    Leave { chat: String },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) if !err.use_stderr() => {
            // --help / --version.
            let _ = err.print();
            return ExitCode::from(EXIT_OK);
        }
        Err(err) if std::env::args_os().any(|a| a == "--json") => {
            let message = err.render().to_string();
            print_error(&CliError::usage(message.trim()), true);
            return ExitCode::from(EXIT_USAGE);
        }
        Err(err) => err.exit(),
    };
    let json = cli.json;
    match run(cli).await {
        Ok(output) => {
            output.print(json);
            ExitCode::from(EXIT_OK)
        }
        Err(err) => {
            print_error(&err, json);
            ExitCode::from(err.kind.exit_code())
        }
    }
}

async fn run(cli: Cli) -> Result<Output> {
    let dir = cli
        .profile
        .ok_or_else(|| CliError::usage("--profile (or GIM_PROFILE) is required"))?;
    let server = cli.server.as_deref();
    let keep = KeepArg::from_args(cli.keep_server.as_deref(), cli.no_keep);
    let keep = keep.as_ref();
    if let Command::Register {
        handle,
        display_name,
    } = &cli.command
    {
        return account::register(&dir, server, keep, handle, display_name).await;
    }
    let command = match cli.command {
        Command::Restore {
            kit_file,
            account_id,
            recovery_key,
            keep_devices,
        } => {
            let args = account::RestoreArgs {
                kit_file,
                account_id,
                recovery_key,
                keep_devices,
            };
            return account::restore(&dir, server, keep, args).await;
        }
        Command::Link { yes } => return account::link(&dir, server, keep, yes).await,
        command => command,
    };
    let ctx = Ctx::connect(&dir, server, keep).await?;
    let output = dispatch(&ctx, command, cli.json).await?;
    ctx.client.flush_outbox().await?;
    Ok(output)
}

async fn dispatch(ctx: &Ctx, command: Command, json: bool) -> Result<Output> {
    match command {
        Command::Register { .. } | Command::Restore { .. } | Command::Link { .. } => {
            unreachable!("handled before connecting")
        }
        Command::Whoami | Command::Profile(ProfileCommand::Show) => account::whoami(ctx),
        Command::Profile(ProfileCommand::Name { name }) => settings::set_name(ctx, &name).await,
        Command::Profile(ProfileCommand::Username(UsernameCommand::Set { username })) => {
            settings::set_username(ctx, &username).await
        }
        Command::Profile(ProfileCommand::Username(UsernameCommand::Remove)) => {
            settings::remove_username(ctx).await
        }
        Command::Devices(DevicesCommand::List) => settings::devices(ctx).await,
        Command::Devices(DevicesCommand::Add { link_code, yes }) => {
            settings::add_device(ctx, &link_code, yes).await
        }
        Command::Devices(DevicesCommand::Remove { device_id }) => {
            settings::remove_device(ctx, &device_id).await
        }
        Command::Servers(ServersCommand::List) => settings::servers(ctx).await,
        Command::Servers(ServersCommand::Add { url }) => settings::add_server(ctx, &url).await,
        Command::Servers(ServersCommand::Remove {
            server_id,
            remove_groups,
        }) => settings::remove_server(ctx, &server_id, remove_groups).await,
        Command::RecoveryKey(RecoveryKeyCommand::Change) => {
            settings::rotate_recovery_key(ctx).await
        }
        Command::Privacy(PrivacyCommand::Show) => settings::privacy(ctx).await,
        Command::Privacy(PrivacyCommand::ReadReceipts { state }) => {
            settings::set_read_receipts(ctx, matches!(state, Switch::On)).await
        }
        Command::Privacy(PrivacyCommand::Typing { state }) => {
            settings::set_typing(ctx, matches!(state, Switch::On)).await
        }
        Command::Block { user } => settings::block(ctx, &user, true).await,
        Command::Unblock { user } => settings::block(ctx, &user, false).await,
        Command::Report {
            user,
            reason,
            note,
            messages,
        } => settings::report(ctx, &user, reason.into(), note, &messages).await,
        Command::Chats(ChatsCommand::Mute { chat, until_ms }) => {
            settings::mute(ctx, &chat, until_ms).await
        }
        Command::Chats(ChatsCommand::Unmute { chat }) => settings::mute(ctx, &chat, Some(0)).await,
        Command::Chats(ChatsCommand::Pin { chat }) => settings::pin(ctx, &chat, true).await,
        Command::Chats(ChatsCommand::Unpin { chat }) => settings::pin(ctx, &chat, false).await,
        Command::Chats(ChatsCommand::Archive { chat }) => settings::archive(ctx, &chat, true).await,
        Command::Chats(ChatsCommand::Unarchive { chat }) => {
            settings::archive(ctx, &chat, false).await
        }
        Command::Users(UsersCommand::Search { query }) => account::search_users(ctx, &query).await,
        Command::Chats(ChatsCommand::List) => chats::list(ctx),
        Command::Dm { handle } => chats::dm(ctx, &handle).await,
        Command::Group(GroupCommand::Create { name, handles }) => {
            chats::group_create(ctx, &name, &handles).await
        }
        Command::Group(GroupCommand::Add { chat, handles }) => {
            chats::group_add(ctx, &chat, &handles).await
        }
        Command::Group(GroupCommand::Remove { chat, handles }) => {
            chats::group_remove(ctx, &chat, &handles).await
        }
        Command::Group(GroupCommand::Rename { chat, name }) => {
            chats::group_rename(ctx, &chat, &name).await
        }
        Command::Group(GroupCommand::Leave { chat }) => chats::group_leave(ctx, &chat).await,
        Command::Members { chat } => chats::members(ctx, &chat),
        Command::Send { chat, text, reply } => {
            messages::send(ctx, &chat, &text, reply.as_deref()).await
        }
        Command::SendImage {
            chat,
            path,
            caption,
            reply,
        } => media::send_media(ctx, &chat, &path, caption, reply.as_deref(), true).await,
        Command::SendFile {
            chat,
            path,
            caption,
            reply,
        } => media::send_media(ctx, &chat, &path, caption, reply.as_deref(), false).await,
        Command::React { msg, emoji, remove } => messages::react(ctx, &msg, &emoji, remove).await,
        Command::Edit { msg, text } => messages::edit(ctx, &msg, &text).await,
        Command::Delete { msg } => messages::delete(ctx, &msg).await,
        Command::Read { chat } => chats::read(ctx, &chat).await,
        Command::History { chat, limit } => messages::history(ctx, &chat, limit),
        Command::Typing { chat, stop } => chats::typing(ctx, &chat, stop).await,
        Command::Download { msg, path } => media::download(ctx, &msg, &path).await,
        Command::Listen(args) => listen::listen(ctx, &args, json).await,
        Command::WaitFor(args) => wait_for::wait_for(ctx, &args).await,
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn help_lists_commands_and_exit_codes() {
        let help = Cli::command().render_long_help().to_string();
        for name in [
            "register",
            "restore",
            "link",
            "whoami",
            "profile",
            "devices",
            "servers",
            "recovery-key",
            "privacy",
            "block",
            "unblock",
            "report",
            "users",
            "chats",
            "dm",
            "group",
            "members",
            "send",
            "send-image",
            "send-file",
            "react",
            "edit",
            "delete",
            "read",
            "history",
            "typing",
            "download",
            "listen",
            "wait-for",
        ] {
            assert!(
                help.contains(&format!("  {name} ")),
                "{name} missing:\n{help}"
            );
        }
        assert!(help.contains("5  server, network or transport error"));
    }

    #[test]
    fn report_takes_a_reason_a_note_and_messages() {
        let cli = Cli::try_parse_from([
            "gim",
            "report",
            "@mallory",
            "--reason",
            "scam",
            "--note",
            "fake prize",
            "--message",
            "m-1",
            "--message",
            "m-2",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Report { ref user, reason: Reason::Scam, ref note, ref messages }
                if user == "@mallory" && note.as_deref() == Some("fake prize") && messages == &["m-1", "m-2"]
        ));
        assert!(Cli::try_parse_from(["gim", "report", "@mallory"]).is_err());
    }

    #[test]
    fn typing_and_members_help_state_their_limits() {
        let mut cli = Cli::command();
        for (name, about) in [("typing", TYPING_ABOUT), ("members", MEMBERS_ABOUT)] {
            let help = cli
                .find_subcommand_mut(name)
                .unwrap()
                .render_long_help()
                .to_string();
            assert!(help.contains(about), "{name}:\n{help}");
        }
        assert!(TYPING_ABOUT.contains("from a fresh process it is a no-op."));
        assert_eq!(
            MEMBERS_ABOUT,
            "Lists devices known from chat membership only."
        );
    }

    #[test]
    fn users_search_help_says_exact_handle_lookup() {
        let mut cli = Cli::command();
        let search = cli
            .find_subcommand_mut("users")
            .and_then(|users| users.find_subcommand_mut("search"))
            .unwrap();
        let help = search.render_long_help().to_string();
        assert!(
            help.contains("Exact handle lookup; substring search is not available yet."),
            "{help}"
        );
    }

    #[test]
    fn listen_and_wait_for_help_document_options_and_exit_codes() {
        let mut cli = Cli::command();
        for (name, options) in [
            ("listen", &["--chat", "--timeout", "--until", "--count"][..]),
            (
                "wait-for",
                &[
                    "--text",
                    "--from",
                    "--msg",
                    "--reaction",
                    "--read-by",
                    "--delivered-to",
                    "--edited",
                    "--deleted",
                    "--timeout",
                ],
            ),
        ] {
            let help = cli
                .find_subcommand_mut(name)
                .unwrap()
                .render_long_help()
                .to_string();
            for option in options {
                assert!(help.contains(option), "{name} {option}:\n{help}");
            }
            assert!(help.contains("Exit codes:"), "{name}:\n{help}");
            assert!(help.contains("3  "), "{name}:\n{help}");
        }
    }

    #[test]
    fn parses_listen_and_wait_for() {
        let cli =
            Cli::try_parse_from(["gim", "listen", "--chat", "crew", "--until", "ok", "--json"])
                .unwrap();
        assert!(cli.json);
        assert!(matches!(
            cli.command,
            Command::Listen(listen::ListenArgs {
                count: None,
                timeout: None,
                ..
            })
        ));
        let err = Cli::try_parse_from(["gim", "listen", "--count", "0"]).unwrap_err();
        assert_eq!(err.exit_code(), i32::from(EXIT_USAGE));
        let err = Cli::try_parse_from(["gim", "wait-for", "crew"]).unwrap_err();
        assert_eq!(err.exit_code(), i32::from(EXIT_USAGE));
        let cli = Cli::try_parse_from(["gim", "wait-for", "crew", "--text", "Split"]).unwrap();
        assert!(matches!(cli.command, Command::WaitFor(ref a) if a.timeout == 30));
    }

    #[test]
    fn parses_global_options_after_the_command() {
        let cli = Cli::try_parse_from([
            "gim",
            "history",
            "crew",
            "--limit",
            "5",
            "--json",
            "--profile",
            "/p",
        ])
        .unwrap();
        assert!(cli.json);
        assert_eq!(cli.profile, Some(PathBuf::from("/p")));
        assert!(matches!(cli.command, Command::History { limit: 5, .. }));
        let cli = Cli::try_parse_from(["gim", "whoami", "--keep-server", "https://k", "--no-keep"])
            .unwrap();
        assert_eq!(
            KeepArg::from_args(cli.keep_server.as_deref(), cli.no_keep),
            Some(KeepArg::Off)
        );
        let err = Cli::try_parse_from(["gim", "group", "create", "crew"]).unwrap_err();
        assert_eq!(err.exit_code(), i32::from(EXIT_USAGE));
    }
}
