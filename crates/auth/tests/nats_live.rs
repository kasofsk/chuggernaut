//! Tier-2: §7.4 credentials enforced by a real operator-mode NATS server.
//!
//! Boots `nats:2-alpine` with a memory resolver built from freshly generated
//! operator/account keys, then proves a work-container credential can do
//! exactly what the spec allows — and nothing more.

use auth::nats::{
    NatsUserSigner, Permissions, account_jwt, operator_jwt, resolver_config, system_account_jwt,
    work_container_permissions,
};
use futures::StreamExt;
use nkeys::KeyPair;
use serde_json::{Value, json};
use std::time::Duration;
use store::{NatsStore, buckets, keys, subjects};
use test_utils::require_nats_config;

#[tokio::test]
async fn scoped_creds_enforced_by_operator_mode_server() {
    let operator = KeyPair::new_operator();
    let sys = KeyPair::new_account();
    let chug = KeyPair::new_account();
    let config = format!(
        "port: 4222\njetstream {{ store_dir: \"/tmp/js\" }}\n{}",
        resolver_config(
            &operator_jwt(&operator, "chuggernaut").unwrap(),
            &sys.public_key(),
            &system_account_jwt(&operator, &sys.public_key()).unwrap(),
            &chug.public_key(),
            &account_jwt(&operator, &chug.public_key(), "CHUG").unwrap(),
        ),
    );
    let server = require_nats_config!(&config);

    let signer = NatsUserSigner::from_account_seed(&chug.seed().unwrap()).unwrap();

    // Dispatcher identity: unrestricted user in the platform account.
    let dispatcher_creds = signer
        .mint_creds("dispatcher", &Permissions::default(), None)
        .unwrap();
    let dispatcher = NatsStore::connect_with_creds(server.url(), &dispatcher_creds)
        .await
        .expect("dispatcher connect");
    dispatcher.ensure_topology().await.unwrap();

    // Seed state: a job record and one operator inbox message.
    let jobs = dispatcher.raw_bucket(buckets::JOBS).await.unwrap();
    jobs.put_json(&keys::job_key("acme", "api", 1), &json!({ "seq": 1 }))
        .await
        .unwrap();
    dispatcher
        .jetstream()
        .publish(
            subjects::channel_inbox("acme", "api", 1),
            r#"{"text":"hi"}"#.into(),
        )
        .await
        .unwrap()
        .await
        .unwrap();

    // Dispatcher-side responder for all req.* submits.
    let client = dispatcher.client().clone();
    let mut sub = dispatcher.client().subscribe("req.>").await.unwrap();
    tokio::spawn(async move {
        while let Some(msg) = sub.next().await {
            if let Some(reply) = msg.reply {
                client.publish(reply, r#"{"ok":true}"#.into()).await.ok();
            }
        }
    });

    // Work-container credential, valid 5 minutes.
    let work_creds = signer
        .mint_creds(
            "job-acme-api-1-work",
            &work_container_permissions("acme", "api", 1),
            Some(chrono::Duration::minutes(5)),
        )
        .unwrap();
    let work = NatsStore::connect_with_creds(server.url(), &work_creds)
        .await
        .expect("work connect");

    // Allowed: read own channel entry, but NOT write it. Containers post
    // through `req.channel.*` so the dispatcher stays the bucket's sole writer
    // and each post becomes durable event history rather than an overwrite.
    let channels = work.raw_bucket(buckets::CHANNELS).await.unwrap();
    let _: Option<Value> = channels
        .get_json(&keys::channel_key("acme", "api", 1))
        .await
        .expect("channel read");
    let denied = tokio::time::timeout(
        Duration::from_secs(3),
        channels.put_json(
            &keys::channel_key("acme", "api", 1),
            &json!({ "update": null }),
        ),
    )
    .await;
    assert!(
        !matches!(denied, Ok(Ok(()))),
        "direct channel KV write must be denied: {denied:?}"
    );

    // Allowed: read own job record (KV direct get).
    let job: Option<Value> = work
        .raw_bucket(buckets::JOBS)
        .await
        .unwrap()
        .get_json(&keys::job_key("acme", "api", 1))
        .await
        .expect("job read");
    assert_eq!(job.unwrap()["seq"], 1);

    // Allowed: poll the operator inbox (ephemeral pull consumer).
    let msgs = work
        .read_subject_after(
            buckets::STREAM_CHANNEL_INBOX,
            &subjects::channel_inbox("acme", "api", 1),
            0,
            8,
        )
        .await
        .expect("inbox poll");
    assert_eq!(msgs.len(), 1);

    // Allowed: work submit round-trips through the responder.
    work.request_with_retry(
        &subjects::work_submit("acme", "api", 1),
        b"{}",
        3,
        Duration::from_millis(200),
    )
    .await
    .expect("work submit");

    // Denied: another job's record — the direct-get publish is dropped.
    let denied = tokio::time::timeout(
        Duration::from_secs(3),
        work.raw_bucket(buckets::JOBS)
            .await
            .unwrap()
            .get_json::<Value>(&keys::job_key("acme", "web", 9)),
    )
    .await;
    assert!(
        !matches!(denied, Ok(Ok(Some(_)))),
        "cross-project job read must not succeed: {denied:?}"
    );

    // Denied: eval submit (same live responder answered work submit above).
    let denied = work
        .request_with_retry(
            &subjects::eval_submit("acme", "api", 1, 7),
            b"{}",
            1,
            Duration::from_millis(200),
        )
        .await;
    assert!(denied.is_err(), "eval submit must be denied for work creds");

    // Denied: reading another project's channel entry.
    let denied = tokio::time::timeout(
        Duration::from_secs(3),
        channels.get_json::<Value>(&keys::channel_key("acme", "web", 9)),
    )
    .await;
    assert!(
        !matches!(denied, Ok(Ok(Some(_)))),
        "cross-project channel read must not succeed: {denied:?}"
    );
}
