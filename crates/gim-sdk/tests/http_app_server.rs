//! `HttpAppServer` and the hub's account deletion against mock servers:
//! request shapes, root signatures and error mapping.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use gim_common::now_ms;
use gim_identity::{RootKey, RootPublicKey, RootRequestSignature};
use gim_sdk::transport::{Transport, TransportError};
use gim_sdk::{
    AbuseReport, AppServer, HttpAppServer, HttpTransport, ReportReason, ReportedMessage, StoredSlot,
};
use serde_json::{json, Value};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

fn root() -> RootKey {
    RootKey::from_seed(&[3; 32])
}

/// Matches requests signed by `root()` over their method, path, query and
/// body, now.
struct SignedByRoot;

impl Match for SignedByRoot {
    fn matches(&self, request: &Request) -> bool {
        let header = |name: &str| {
            request
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let (Some(key), Some(timestamp), Some(signature)) = (
            header("Gim-Key"),
            header("Gim-Timestamp"),
            header("Gim-Signature"),
        ) else {
            return false;
        };
        let key = URL_SAFE_NO_PAD.decode(key).unwrap();
        let signed = RootRequestSignature {
            public_key: RootPublicKey::from_bytes(&key.try_into().unwrap()).unwrap(),
            timestamp_ms: timestamp.parse().unwrap(),
            signature: URL_SAFE_NO_PAD
                .decode(signature)
                .unwrap()
                .try_into()
                .unwrap(),
        };
        let target = match request.url.query() {
            Some(query) => format!("{}?{query}", request.url.path()),
            None => request.url.path().to_string(),
        };
        signed
            .verify(request.method.as_str(), &target, &request.body, now_ms())
            .is_ok_and(|user_id| user_id == root().public().user_id())
    }
}

fn ok(data: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "success": true, "data": data }))
}

fn app_server(server: &MockServer, prefix: &str) -> HttpAppServer {
    HttpAppServer::new(format!("{}{prefix}", server.uri()).parse().unwrap()).unwrap()
}

#[tokio::test]
async fn slots_are_read_and_written_signed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/raw/gim/v1/storage/privacy"))
        .and(SignedByRoot)
        .respond_with(ok(
            json!({ "version": 4, "data": STANDARD.encode(b"sealed") }),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/raw/gim/v1/storage/privacy"))
        .and(body_json(
            json!({ "version": 4, "data": STANDARD.encode(b"next") }),
        ))
        .and(SignedByRoot)
        .respond_with(ok(json!({ "version": 5 })))
        .expect(1)
        .mount(&server)
        .await;
    let app = app_server(&server, "");
    assert_eq!(
        app.slot(&root(), "privacy").await.unwrap(),
        Some(StoredSlot {
            version: 4,
            data: b"sealed".to_vec()
        })
    );
    assert_eq!(
        app.put_slot(&root(), "privacy", 4, b"next").await.unwrap(),
        5
    );
}

#[tokio::test]
async fn a_missing_slot_is_none_and_a_stale_write_a_conflict() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "success": false,
            "error": { "code": "NOT_FOUND", "message": "Slot not found" }
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "success": false,
            "error": { "code": "VERSION_CONFLICT", "message": "Slot is at version 2", "details": { "version": 2 } }
        })))
        .mount(&server)
        .await;
    let app = app_server(&server, "");
    assert_eq!(app.slot(&root(), "privacy").await.unwrap(), None);
    assert!(matches!(
        app.put_slot(&root(), "privacy", 1, b"x").await,
        Err(TransportError::Conflict(_))
    ));
}

#[tokio::test]
async fn a_report_and_the_data_deletion_are_signed_below_a_base_path() {
    let server = MockServer::start().await;
    let report = AbuseReport {
        reported_user_id: RootKey::from_seed(&[4; 32]).public().user_id().to_string(),
        reason: ReportReason::Harassment,
        note: None,
        messages: vec![ReportedMessage {
            message_id: "m1".into(),
            sender_user_id: RootKey::from_seed(&[4; 32]).public().user_id().to_string(),
            sent_at_ms: 7,
            text: "go away".into(),
        }],
    };
    Mock::given(method("POST"))
        .and(path("/app/api/raw/gim/v1/reports"))
        .and(body_json(json!({
            "reportedUserId": report.reported_user_id,
            "reason": "harassment",
            "messages": [{
                "messageId": "m1",
                "senderUserId": report.reported_user_id,
                "sentAtMs": 7,
                "text": "go away",
            }],
        })))
        .and(SignedByRoot)
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({ "success": true, "data": { "id": "r1" } })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/app/api/raw/gim/v1/me"))
        .and(SignedByRoot)
        .respond_with(ok(json!({ "deletedSlots": 1 })))
        .expect(1)
        .mount(&server)
        .await;
    let app = app_server(&server, "/app");
    assert_eq!(app.report(&root(), &report).await.unwrap(), "r1");
    app.delete_user_data(&root()).await.unwrap();
}

#[tokio::test]
async fn the_hub_deletes_the_account_on_a_root_signed_request() {
    let server = MockServer::start().await;
    let user_id = root().public().user_id();
    Mock::given(method("DELETE"))
        .and(path(format!("/v2/users/{user_id}")))
        .and(SignedByRoot)
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let hub = HttpTransport::new(server.uri().parse().unwrap()).unwrap();
    hub.delete_user(&root()).await.unwrap();

    let gone = MockServer::start().await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": { "code": "not_found", "message": "no such user" }
        })))
        .mount(&gone)
        .await;
    let hub = HttpTransport::new(gone.uri().parse().unwrap()).unwrap();
    assert!(matches!(
        hub.delete_user(&root()).await,
        Err(TransportError::NotFound(_))
    ));
}
