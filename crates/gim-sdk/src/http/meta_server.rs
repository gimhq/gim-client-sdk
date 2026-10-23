//! [`MetaServer`] over the gim-meta raw API (`/api/raw/gim/v1`): JSON bodies
//! in the `{ success, data }` envelope, every request root-signed.

use async_trait::async_trait;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use gim_common::now_ms;
use gim_identity::RootKey;
use reqwest::header::CONTENT_TYPE;
use reqwest::{Method, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::{error, retry_after, HttpConfig, Url};
use crate::meta_server::{AbuseReport, MetaServer, StoredSlot};
use crate::transport::{TransportError, TransportResult};

/// Route prefix of the client API on the meta server.
const API_PREFIX: &str = "api/raw/gim/v1/";

/// HTTP [`MetaServer`]. Clones share the connection pool.
#[derive(Debug, Clone)]
pub struct HttpMetaServer {
    client: reqwest::Client,
    /// Ends with `API_PREFIX`.
    api_url: Url,
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Serialize, Deserialize)]
struct Slot {
    version: u64,
    data: String,
}

#[derive(Deserialize)]
struct Written {
    version: u64,
}

#[derive(Deserialize)]
struct Created {
    id: String,
}

impl HttpMetaServer {
    /// `base_url` is the meta server's origin (and path, if it is mounted
    /// below one); the client API is under `api/raw/gim/v1` there.
    pub fn new(base_url: Url) -> TransportResult<Self> {
        Self::with_config(base_url, HttpConfig::default())
    }

    pub fn with_config(mut base_url: Url, config: HttpConfig) -> TransportResult<Self> {
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        let api_url = base_url
            .join(API_PREFIX)
            .map_err(|e| TransportError::Network(format!("invalid meta server URL: {e}")))?;
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .build()
            .map_err(error::from_reqwest)?;
        Ok(Self { client, api_url })
    }

    /// Sends a request signed with `root` over the URL's path and query
    /// as sent; a non-2xx status becomes an error.
    async fn send(
        &self,
        root: &RootKey,
        method: Method,
        route: &str,
        body: Option<Vec<u8>>,
    ) -> TransportResult<Response> {
        let url = self
            .api_url
            .join(route)
            .map_err(|e| TransportError::Network(format!("invalid meta server route: {e}")))?;
        let path = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_string(),
        };
        let body = body.unwrap_or_default();
        let signed = root.sign_request(method.as_str(), &path, now_ms(), &body);
        let mut request = self
            .client
            .request(method, url)
            .header(
                "Gim-Key",
                URL_SAFE_NO_PAD.encode(signed.public_key.as_bytes()),
            )
            .header("Gim-Timestamp", signed.timestamp_ms.to_string())
            .header("Gim-Signature", URL_SAFE_NO_PAD.encode(signed.signature));
        if !body.is_empty() {
            request = request.header(CONTENT_TYPE, "application/json").body(body);
        }
        let response = request.send().await.map_err(error::from_reqwest)?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let retry_after = retry_after(&response);
        let body = response.bytes().await.unwrap_or_default();
        Err(error::from_status(status, retry_after, &body))
    }

    async fn data<T: DeserializeOwned>(response: Response) -> TransportResult<T> {
        let envelope: Envelope<T> = response.json().await.map_err(error::from_reqwest)?;
        Ok(envelope.data)
    }
}

fn json(value: &impl Serialize) -> TransportResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(|e| TransportError::Network(e.to_string()))
}

#[async_trait]
impl MetaServer for HttpMetaServer {
    async fn slot(&self, root: &RootKey, name: &str) -> TransportResult<Option<StoredSlot>> {
        let route = format!("storage/{name}");
        let slot: Slot = match self.send(root, Method::GET, &route, None).await {
            Ok(response) => Self::data(response).await?,
            Err(TransportError::NotFound(_)) => return Ok(None),
            Err(err) => return Err(err),
        };
        let data = STANDARD
            .decode(slot.data)
            .map_err(|e| TransportError::Server(format!("slot {name}: {e}")))?;
        Ok(Some(StoredSlot {
            version: slot.version,
            data,
        }))
    }

    async fn put_slot(
        &self,
        root: &RootKey,
        name: &str,
        version: u64,
        data: &[u8],
    ) -> TransportResult<u64> {
        let body = json(&Slot {
            version,
            data: STANDARD.encode(data),
        })?;
        let route = format!("storage/{name}");
        let response = self.send(root, Method::PUT, &route, Some(body)).await?;
        Ok(Self::data::<Written>(response).await?.version)
    }

    async fn report(&self, root: &RootKey, report: &AbuseReport) -> TransportResult<String> {
        let response = self
            .send(root, Method::POST, "reports", Some(json(report)?))
            .await?;
        Ok(Self::data::<Created>(response).await?.id)
    }

    async fn delete_user_data(&self, root: &RootKey) -> TransportResult<()> {
        self.send(root, Method::DELETE, "me", None).await?;
        Ok(())
    }
}
