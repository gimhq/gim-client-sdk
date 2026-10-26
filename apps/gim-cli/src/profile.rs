//! Profile directory: `gim.sqlite3` (SQLCipher store), `secrets.json`
//! (session and database key), `config.json` (server and keep server URLs),
//! `tmp/` (media temp files).

use std::fs;
use std::io::ErrorKind as IoErrorKind;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gim_base::BaseError;
use gim_sdk::http::Url;
use gim_sdk::{Client, ClientBuilder, HttpKeepServer, HttpTransport, Session};
use gim_store_sqlite::{DbKey, SqliteStore};
use serde::{Deserialize, Serialize};

use crate::error::{CliError, ErrorKind, Result};
use crate::secrets::{FileSecretStorage, SECRETS_FILE};

pub const CONFIG_FILE: &str = "config.json";

/// The built-in server (hub), used when neither `--server`, `GIM_SERVER`
/// nor the profile names one.
pub const DEFAULT_SERVER: &str = "https://hub.g.im";
/// The built-in keep server (encrypted settings backup, reports), used
/// unless the profile names another one or turned it off.
pub const DEFAULT_KEEP_SERVER: &str = "https://keep.g.im";
/// `--keep-server` value that goes back to [`DEFAULT_KEEP_SERVER`].
pub const KEEP_SERVER_DEFAULT_ARG: &str = "default";

pub type GimClient = Client<SqliteStore>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Another keep server than [`DEFAULT_KEEP_SERVER`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_server: Option<String>,
    /// The keep server is off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_keep: bool,
}

/// The command line's keep server choice (`--keep-server`, `GIM_KEEP_SERVER`
/// or `--no-keep`), saved to the profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepArg {
    Url(String),
    /// Back to [`DEFAULT_KEEP_SERVER`].
    Default,
    Off,
}

impl KeepArg {
    /// `--no-keep` wins over a URL (which may come from the environment).
    pub fn from_args(keep_server: Option<&str>, no_keep: bool) -> Option<Self> {
        if no_keep {
            return Some(Self::Off);
        }
        keep_server.map(|url| match url {
            KEEP_SERVER_DEFAULT_ARG => Self::Default,
            url => Self::Url(url.to_string()),
        })
    }
}

impl Config {
    /// The keep server to use; `None` when it is off.
    pub fn keep_server(&self) -> Option<String> {
        if self.no_keep {
            return None;
        }
        Some(
            self.keep_server
                .clone()
                .unwrap_or_else(|| DEFAULT_KEEP_SERVER.to_string()),
        )
    }

    pub fn set_keep(&mut self, keep: &KeepArg) {
        (self.keep_server, self.no_keep) = match keep {
            KeepArg::Url(url) => (Some(url.clone()), false),
            KeepArg::Default => (None, false),
            KeepArg::Off => (None, true),
        };
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(CONFIG_FILE);
        match fs::read(&path) {
            Ok(data) => serde_json::from_slice(&data)
                .map_err(|e| CliError::other(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == IoErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = dir.join(CONFIG_FILE);
        let tmp = dir.join(format!("{CONFIG_FILE}.tmp"));
        let data = serde_json::to_vec_pretty(self).expect("config serializes");
        fs::write(&tmp, data)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }
}

pub struct Profile {
    pub dir: PathBuf,
    pub config: Config,
}

impl Profile {
    /// Opens the profile, creating its directory (0700) if needed.
    pub fn create(dir: &Path) -> Result<Self> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        Self::load(dir)
    }

    /// Opens an existing profile; a missing directory is an unregistered
    /// profile.
    pub fn open(dir: &Path) -> Result<Self> {
        if !dir.is_dir() {
            return Err(CliError::not_registered());
        }
        Self::load(dir)
    }

    fn load(dir: &Path) -> Result<Self> {
        let dir = std::path::absolute(dir)?;
        let config = Config::load(&dir)?;
        Ok(Self { dir, config })
    }

    /// `--server`, else the configured server, else [`DEFAULT_SERVER`].
    pub fn server(&self, server: Option<&str>) -> String {
        server
            .map(str::to_string)
            .or_else(|| self.config.server.clone())
            .unwrap_or_else(|| DEFAULT_SERVER.to_string())
    }

    /// Saves the command line's keep server choice, if any.
    pub fn apply_keep(&mut self, keep: Option<&KeepArg>) -> Result<()> {
        if let Some(keep) = keep {
            self.config.set_keep(keep);
            self.config.save(&self.dir)?;
        }
        Ok(())
    }

    /// A client without a session, with the profile's keep server.
    pub fn client(&self, server: &str) -> Result<GimClient> {
        let url = Url::parse(server)
            .map_err(|e| CliError::usage(format!("invalid server URL {server:?}: {e}")))?;
        let transport =
            HttpTransport::new(url).map_err(|e| CliError::new(ErrorKind::Server, e.to_string()))?;
        let keep_server = match self.config.keep_server() {
            Some(keep) => {
                let url = Url::parse(&keep).map_err(|e| {
                    CliError::usage(format!("invalid keep server URL {keep:?}: {e}"))
                })?;
                Some(
                    HttpKeepServer::new(url)
                        .map_err(|e| CliError::new(ErrorKind::Server, e.to_string()))?,
                )
            }
            None => None,
        };
        let secrets = FileSecretStorage::new(self.dir.join(SECRETS_FILE));
        let mut reset = false;
        let builder =
            ClientBuilder::open(Arc::new(transport), Arc::new(secrets), |key, new_key| {
                let opened = SqliteStore::open_or_reset(&self.dir, &DbKey::new(*key), new_key)
                    .map_err(BaseError::from)?;
                reset = opened.1;
                Ok::<_, BaseError>(opened)
            })?;
        if reset {
            // An M1 plaintext database (or one the stored key cannot
            // decrypt) was wiped.
            eprintln!(
                "gim: local database in {} was reset; local state is lost",
                self.dir.display()
            );
        }
        let builder = builder.data_dir(&self.dir);
        let builder = match keep_server {
            Some(keep_server) => builder.keep_server(Arc::new(keep_server)),
            None => builder,
        };
        Ok(builder.build()?)
    }
}

/// A registered client with its session restored and the store caught up.
pub struct Ctx {
    pub client: GimClient,
    pub session: Session,
    pub server: String,
    /// `None` when the keep server is off.
    pub keep_server: Option<String>,
}

impl Ctx {
    pub async fn connect(dir: &Path, server: Option<&str>, keep: Option<&KeepArg>) -> Result<Self> {
        let mut profile = Profile::open(dir)?;
        profile.apply_keep(keep)?;
        let server = profile.server(server);
        let client = profile.client(&server)?;
        if !client.restore_session().await? {
            return Err(CliError::not_registered());
        }
        let session = client.session().ok_or_else(CliError::not_registered)?;
        // Reads must see the server's state: a failed sync fails the command.
        client.sync_once().await.map_err(|e| {
            let err = CliError::from(e);
            let kind = match err.kind {
                ErrorKind::NotFound => ErrorKind::NotFound,
                _ => ErrorKind::Server,
            };
            CliError::new(kind, format!("sync failed: {}", err.message))
        })?;
        Ok(Self {
            client,
            session,
            server,
            keep_server: profile.config.keep_server(),
        })
    }

    pub fn store(&self) -> &SqliteStore {
        self.client.store()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(dir.path()).unwrap(), Config::default());
        let config = Config {
            server: Some("https://gim.example.eu".into()),
            ..Config::default()
        };
        config.save(dir.path()).unwrap();
        assert_eq!(Config::load(dir.path()).unwrap(), config);
        let raw: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.path().join(CONFIG_FILE)).unwrap()).unwrap();
        assert_eq!(raw, serde_json::json!({"server": "https://gim.example.eu"}));
    }

    #[test]
    fn profile_dir_is_owner_only_and_server_override_wins() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("alice");
        assert_eq!(
            Profile::open(&dir).err().map(|e| e.kind),
            Some(ErrorKind::NotFound)
        );
        let profile = Profile::create(&dir).unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert_eq!(profile.server(None), DEFAULT_SERVER);
        assert_eq!(profile.server(Some("http://b")), "http://b");
        Config {
            server: Some("http://a".into()),
            ..Config::default()
        }
        .save(&dir)
        .unwrap();
        let profile = Profile::open(&dir).unwrap();
        assert_eq!(profile.server(None), "http://a");
        assert_eq!(profile.server(Some("http://b")), "http://b");
    }

    #[test]
    fn keep_server_defaults_is_saved_turned_off_and_reset() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("alice");
        let mut profile = Profile::create(&dir).unwrap();
        assert_eq!(
            profile.config.keep_server().as_deref(),
            Some(DEFAULT_KEEP_SERVER)
        );
        // No choice on the command line: nothing is written.
        profile.apply_keep(None).unwrap();
        assert!(!dir.join(CONFIG_FILE).exists());

        let other = KeepArg::Url("https://keep.example.eu".into());
        profile.apply_keep(Some(&other)).unwrap();
        let profile = Profile::open(&dir).unwrap();
        assert_eq!(
            profile.config.keep_server().as_deref(),
            Some("https://keep.example.eu")
        );

        let mut profile = profile;
        profile.apply_keep(Some(&KeepArg::Off)).unwrap();
        let mut profile = Profile::open(&dir).unwrap();
        assert_eq!(profile.config.keep_server(), None);
        let raw: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join(CONFIG_FILE)).unwrap()).unwrap();
        assert_eq!(raw, serde_json::json!({"no_keep": true}));
        drop(profile.client("http://127.0.0.1:1").unwrap());

        profile.apply_keep(Some(&KeepArg::Default)).unwrap();
        let profile = Profile::open(&dir).unwrap();
        assert_eq!(profile.config, Config::default());
        assert_eq!(
            profile.config.keep_server().as_deref(),
            Some(DEFAULT_KEEP_SERVER)
        );
    }

    #[test]
    fn keep_arg_no_keep_wins_and_default_resets() {
        assert_eq!(KeepArg::from_args(None, false), None);
        assert_eq!(
            KeepArg::from_args(Some("https://k"), false),
            Some(KeepArg::Url("https://k".into()))
        );
        assert_eq!(
            KeepArg::from_args(Some("default"), false),
            Some(KeepArg::Default)
        );
        assert_eq!(
            KeepArg::from_args(Some("https://k"), true),
            Some(KeepArg::Off)
        );
        assert_eq!(KeepArg::from_args(None, true), Some(KeepArg::Off));
    }

    #[test]
    fn an_invalid_keep_server_url_is_a_usage_error() {
        let root = tempfile::tempdir().unwrap();
        let mut profile = Profile::create(&root.path().join("alice")).unwrap();
        profile
            .apply_keep(Some(&KeepArg::Url("not a url".into())))
            .unwrap();
        assert_eq!(
            profile.client("http://127.0.0.1:1").err().map(|e| e.kind),
            Some(ErrorKind::Usage)
        );
    }

    #[test]
    fn client_keys_the_store_from_secrets_and_wipes_an_unreadable_db() {
        use gim_sdk::SecretStorage;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("alice");
        let profile = Profile::create(&dir).unwrap();
        let db = dir.join(gim_store_sqlite::DATABASE_FILE_NAME);
        // Stands in for an M1 plaintext database.
        fs::write(&db, b"not an encrypted database").unwrap();
        drop(profile.client("http://127.0.0.1:1").unwrap());
        assert_ne!(fs::read(&db).unwrap(), b"not an encrypted database");
        let secrets = FileSecretStorage::new(dir.join(SECRETS_FILE));
        let key = secrets
            .get(ClientBuilder::<SqliteStore>::DB_KEY_SECRET_KEY)
            .unwrap()
            .unwrap();
        assert_eq!(key.len(), ClientBuilder::<SqliteStore>::DB_KEY_LEN);
        // Reopening with the stored key keeps the database and the key.
        let encrypted = fs::read(&db).unwrap();
        drop(profile.client("http://127.0.0.1:1").unwrap());
        assert_eq!(fs::read(&db).unwrap()[..16], encrypted[..16]);
        assert_eq!(
            secrets
                .get(ClientBuilder::<SqliteStore>::DB_KEY_SECRET_KEY)
                .unwrap(),
            Some(key)
        );
    }
}
