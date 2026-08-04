//! Tier-2: the private-server contract `NatsTestServer::spawn` promises (#408).
//!
//! Its own binary, so the process-wide `CHUG_TEST_NATS_LOCAL=0` the #407 unit
//! test sets cannot reach it. Self-skips when neither a local `nats-server` nor
//! Docker can serve, like every other tier-2 file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use test_utils::nats::NatsTestServer;

/// Two private servers must be genuinely private: distinct ports, and a stream
/// created in one is invisible to the other.
#[tokio::test]
async fn two_private_servers_share_no_port_and_no_state() {
    let Some(first) = NatsTestServer::spawn().await else {
        return;
    };
    let Some(second) = NatsTestServer::spawn().await else {
        return;
    };
    assert_ne!(first.url(), second.url());

    let first_store = store::NatsStore::connect(first.url()).await.unwrap();
    first_store.ensure_topology().await.unwrap();
    first_store
        .raw_bucket(store::buckets::JOBS)
        .await
        .unwrap()
        .put_json("acme.api.1", &serde_json::json!({ "seq": 1 }))
        .await
        .unwrap();

    let second_store = store::NatsStore::connect(second.url()).await.unwrap();
    second_store.ensure_topology().await.unwrap();
    let leaked: Option<serde_json::Value> = second_store
        .raw_bucket(store::buckets::JOBS)
        .await
        .unwrap()
        .get_json("acme.api.1")
        .await
        .unwrap();
    assert!(
        leaked.is_none(),
        "a private server saw another's state: {leaked:?}"
    );
}

/// Dropping the server takes its port with it, so ~20 spawns in one binary
/// cannot pile up processes.
#[tokio::test]
async fn a_dropped_private_server_releases_its_port() {
    let Some(server) = NatsTestServer::spawn().await else {
        return;
    };
    let hostport = server.url().strip_prefix("nats://").unwrap().to_string();
    assert!(tokio::net::TcpStream::connect(&hostport).await.is_ok());
    drop(server);

    for _ in 0..100u32 {
        if tokio::net::TcpStream::connect(&hostport).await.is_err() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("{hostport} still accepts connections after the server was dropped");
}
