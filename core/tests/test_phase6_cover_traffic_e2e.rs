use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bitchat2_core::api::{BitchatCoreService, BridgeMessage};
use bitchat2_core::identity::Identity;
use bitchat2_core::storage::{InjectedKeyProvider, KeyProvider};
use rusqlite::Connection;

struct CoverNode {
    dir: PathBuf,
    svc: BitchatCoreService,
    inbox: Vec<BridgeMessage>,
}

impl CoverNode {
    async fn new(tag: &str, key_byte: u8) -> Self {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("bitchat_cov_{tag}_{ts}_{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let kp: Arc<dyn KeyProvider> = Arc::new(InjectedKeyProvider::new([key_byte; 32], 1));
        let svc = BitchatCoreService::new_with_key_provider(dir.to_string_lossy().into_owned(), kp)
            .await
            .unwrap();
        Self {
            dir,
            svc,
            inbox: Vec::new(),
        }
    }

    fn pid_hex(&self) -> String {
        self.svc.my_peer_id().to_string()
    }

    fn short_hex(&self) -> String {
        self.svc.my_short_id().to_string()
    }

    async fn pump(&mut self) {
        let _ = self.svc.pump_outbox().await;
        for _ in 0..32 {
            match self.svc.poll_inbound().await {
                Ok(Some(m)) => self.inbox.push(m),
                _ => break,
            }
        }
    }

    fn cover_status(&self) -> serde_json::Value {
        serde_json::from_str(&self.svc.get_cover_status()).unwrap()
    }

    fn sqlite_outbox_rows(&self) -> i64 {
        let conn = Connection::open(self.dir.join("bitchat_outbox.db")).unwrap();
        conn.query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0))
            .unwrap()
    }

    async fn cleanup(mut self) {
        let _ = self.svc.gateway_stop().await;
        drop(self.svc);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn connect_pair(a: &mut CoverNode, b: &mut CoverNode, approve: bool) {
    let a_sh = a.short_hex();
    let b_sh = b.short_hex();
    a.svc.on_peer_connected(b_sh.clone()).await.unwrap();
    b.svc.on_peer_connected(a_sh.clone()).await.unwrap();
    if approve {
        a.svc
            .trust_set_peer_state(b.pid_hex(), "approve".into())
            .await
            .unwrap();
        b.svc
            .trust_set_peer_state(a.pid_hex(), "approve".into())
            .await
            .unwrap();
    }
    for _ in 0..30 {
        while let Some(f) = a.svc.poll_outbound() {
            let _ = b.svc.on_frame_received(a_sh.clone(), f.bytes);
        }
        while let Some(f) = b.svc.poll_outbound() {
            let _ = a.svc.on_frame_received(b_sh.clone(), f.bytes);
        }
        a.pump().await;
        b.pump().await;
        if a.svc.is_session_ready(&b_sh).unwrap() && b.svc.is_session_ready(&a_sh).unwrap() {
            break;
        }
    }
    assert!(a.svc.is_session_ready(&b_sh).unwrap());
    assert!(b.svc.is_session_ready(&a_sh).unwrap());
}

async fn exchange_once(a: &mut CoverNode, b: &mut CoverNode) -> (usize, usize) {
    let a_sh = a.short_hex();
    let b_sh = b.short_hex();
    let mut a_tx = 0usize;
    let mut b_tx = 0usize;
    while let Some(f) = a.svc.poll_outbound() {
        let _ = b.svc.on_frame_received(a_sh.clone(), f.bytes);
        a_tx += 1;
    }
    b.pump().await;
    while let Some(f) = b.svc.poll_outbound() {
        let _ = a.svc.on_frame_received(b_sh.clone(), f.bytes);
        b_tx += 1;
    }
    a.pump().await;
    (a_tx, b_tx)
}

#[tokio::test]
async fn test_t7_cover_default_off_unsupported_and_no_absent_session_emission() {
    let mut alice = CoverNode::new("t7_off_a", 0x11).await;
    let mut bob = CoverNode::new("t7_off_b", 0x12).await;

    // Default OFF
    assert_eq!(alice.cover_status()["enabled"].as_bool(), Some(false));
    assert_eq!(alice.cover_status()["experimental"].as_bool(), Some(true));

    // Absent session -> even when enabled, pumping produces 0 outbound frames
    alice.svc.configure_cover_policy(20, 20, 1024);
    alice.svc.set_cover_traffic_enabled(true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    alice.pump().await;
    assert!(alice.svc.poll_outbound().is_none());
    assert_eq!(alice.cover_status()["dummy_frames_sent"].as_u64(), Some(0));

    // Connect approved pair, Bob remains Default OFF (unsupported)
    connect_pair(&mut alice, &mut bob, true).await;
    exchange_once(&mut alice, &mut bob).await;
    exchange_once(&mut alice, &mut bob).await;

    tokio::time::sleep(Duration::from_millis(30)).await;
    alice.pump().await;
    let (a_tx, b_tx) = exchange_once(&mut alice, &mut bob).await;
    assert_eq!(a_tx, 0, "Unsupported peer must receive zero cover dummies");
    assert_eq!(b_tx, 0);
    assert_eq!(alice.cover_status()["dummy_frames_sent"].as_u64(), Some(0));

    alice.cleanup().await;
    bob.cleanup().await;
}

#[tokio::test]
async fn test_t7_cover_negotiated_silent_discard_no_ack_storm_mode_pause_budget_and_priority() {
    let mut alice = CoverNode::new("t7_on_a", 0x21).await;
    let mut bob = CoverNode::new("t7_on_b", 0x22).await;
    connect_pair(&mut alice, &mut bob, true).await;

    alice.svc.configure_cover_policy(20, 30, 512);
    bob.svc.configure_cover_policy(60_000, 60_000, 512); // Bob accepts Offers without flooding back
    alice.svc.set_cover_traffic_enabled(true).await.unwrap();
    bob.svc.set_cover_traffic_enabled(true).await.unwrap();

    // Complete Offer <-> Accept negotiation
    for _ in 0..4 {
        exchange_once(&mut alice, &mut bob).await;
    }
    assert_eq!(alice.cover_status()["negotiated_peers"].as_u64(), Some(1));
    assert_eq!(bob.cover_status()["negotiated_peers"].as_u64(), Some(1));

    // Pump across ticks to trigger bounded dummy emission & verify silent discard without ACKs or UI leaks
    let mut observed_dummies = 0usize;
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(35)).await;
        alice.pump().await;
        let (a_tx, b_tx) = exchange_once(&mut alice, &mut bob).await;
        observed_dummies += a_tx;
        assert_eq!(b_tx, 0, "Dummy discard must never amplify or send ACKs");
    }
    assert!(observed_dummies >= 1);
    let st_a = alice.cover_status();
    let st_b = bob.cover_status();
    assert!(st_a["dummy_frames_sent"].as_u64().unwrap() >= 1);
    assert!(st_a["dummy_bytes_sent"].as_u64().unwrap() <= 512);
    assert!(st_b["dummy_frames_discarded"].as_u64().unwrap() >= 1);
    assert!(
        bob.inbox.is_empty(),
        "Dummies must never surface in UI inbox"
    );
    assert_eq!(
        alice.sqlite_outbox_rows(),
        0,
        "Dummies must never touch SQLite outbox"
    );
    assert_eq!(bob.sqlite_outbox_rows(), 0);
    assert!(alice.svc.poll_delivery_events_vec().is_empty());

    // Budget exhaustion & resource pause override
    assert_eq!(
        alice
            .svc
            .set_relay_contribution_profile("receive_only")
            .unwrap(),
        "Paused"
    );
    let before_pause = alice.cover_status()["dummy_frames_sent"].as_u64().unwrap();
    tokio::time::sleep(Duration::from_millis(35)).await;
    alice.pump().await;
    assert_eq!(
        alice.cover_status()["dummy_frames_sent"].as_u64().unwrap(),
        before_pause,
        "Paused resource profile must immediately halt cover traffic"
    );
    alice
        .svc
        .set_relay_contribution_profile("balanced")
        .unwrap();

    // Stop / start idempotent + Disable notice sent cleanly + real traffic priority
    alice.svc.set_cover_traffic_enabled(false).await.unwrap();
    alice.svc.set_cover_traffic_enabled(false).await.unwrap();
    exchange_once(&mut alice, &mut bob).await;
    assert_eq!(alice.cover_status()["negotiated_peers"].as_u64(), Some(0));

    // Re-enable with generous budget, queue real message -> real message dispatches immediately & never blocked by cover!
    alice.svc.configure_cover_policy(20, 20, 4096);
    alice.svc.set_cover_traffic_enabled(true).await.unwrap();
    for _ in 0..3 {
        exchange_once(&mut alice, &mut bob).await;
    }
    let lid = alice
        .svc
        .send_message(bob.pid_hex(), "real-priority-msg".into(), 0)
        .await
        .unwrap();
    for _ in 0..4 {
        exchange_once(&mut alice, &mut bob).await;
    }
    assert_eq!(
        alice.svc.get_delivery_state(lid).await.unwrap(),
        "Delivered"
    );
    assert_eq!(bob.inbox.len(), 1);
    assert_eq!(bob.inbox[0].text, "real-priority-msg");

    let _ = Identity::generate();
    alice.cleanup().await;
    bob.cleanup().await;
}
