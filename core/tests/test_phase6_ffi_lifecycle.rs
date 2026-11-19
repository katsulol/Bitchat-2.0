use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use bitchat2_core::api::{self, DiagnosticsSnapshot, CORE_ABI_VERSION, CORE_BUILD_VERSION};
use bitchat2_core::ffi::{
    bitchat_abi_version, bitchat_attach_ui, bitchat_build_info, bitchat_check_abi_version,
    bitchat_destructive_reset_storage, bitchat_detach_ui, bitchat_free_bytes, bitchat_free_string,
    bitchat_gateway_start, bitchat_gateway_stop, bitchat_get_diagnostics, bitchat_init_with_key,
    bitchat_last_error, bitchat_on_frame_received, bitchat_on_frame_received_gen,
    bitchat_on_peer_connected_gen, bitchat_on_peer_disconnected_gen,
    bitchat_pending_message_requests, bitchat_poll_auth_events, bitchat_poll_inbound,
    bitchat_poll_outbound, bitchat_runtime_generation, bitchat_send_broadcast,
    bitchat_send_message, bitchat_shutdown, MAX_FFI_FRAME_LEN, MAX_FFI_STRING_LEN,
};
use bitchat2_core::storage::{InjectedKeyProvider, KeyProvider};

static FFI_TEST_LOCK: Mutex<()> = Mutex::new(());

fn take_c_string(ptr: *mut c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe { bitchat_free_string(ptr) };
    Some(s)
}

fn fresh_temp_dir(prefix: &str) -> std::path::PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("bitchat_{prefix}_{ts}_{}", rand::random::<u32>()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn rt_block<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

#[test]
fn test_t5_ffi_lifecycle_generations_validation_and_abi() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    rt_block(async {
        let _ = api::shutdown().await;
    });

    // 1. Provenance & ABI version checking
    unsafe {
        assert_eq!(bitchat_abi_version(), CORE_ABI_VERSION);
        assert_eq!(bitchat_check_abi_version(CORE_ABI_VERSION), 0);
        assert_eq!(bitchat_check_abi_version(CORE_ABI_VERSION + 77), -1);
        let mismatch_err =
            take_c_string(bitchat_last_error()).expect("ABI mismatch must set last_error");
        assert!(
            mismatch_err.contains("ABI_MISMATCH"),
            "Unexpected error string: {mismatch_err}"
        );

        let build_info = take_c_string(bitchat_build_info()).expect("build info");
        let parsed: serde_json::Value = serde_json::from_str(&build_info).unwrap();
        assert_eq!(parsed["build_version"].as_str(), Some(CORE_BUILD_VERSION));
        assert_eq!(
            parsed["abi_version"].as_u64(),
            Some(CORE_ABI_VERSION as u64)
        );
        assert!(!parsed["git_commit"].as_str().unwrap_or("").is_empty());
    }

    let dir = fresh_temp_dir("t5_ffi_life");
    let c_path = CString::new(dir.to_str().unwrap()).unwrap();
    let dek = [0x42u8; 32];

    unsafe {
        // 2. First init + idempotent double init
        let s1 = take_c_string(bitchat_init_with_key(c_path.as_ptr(), dek.as_ptr(), 1))
            .expect("first init succeeds");
        let gen1 = bitchat_runtime_generation();
        assert!(gen1 > 0);
        assert_eq!(s1.len(), 16);

        let s2 = take_c_string(bitchat_init_with_key(c_path.as_ptr(), dek.as_ptr(), 1))
            .expect("second init is idempotent");
        assert_eq!(s1, s2);
        assert_eq!(
            bitchat_runtime_generation(),
            gen1,
            "idempotent init must keep active generation"
        );

        // 3. UI attach / detach & diagnostics reflection
        assert_eq!(bitchat_attach_ui(), gen1);
        let diag_str = take_c_string(bitchat_get_diagnostics()).expect("diagnostics");
        let diag: DiagnosticsSnapshot = serde_json::from_str(&diag_str).unwrap();
        assert!(diag.ui_attached);
        assert_eq!(diag.runtime_generation, gen1);
        assert_eq!(diag.abi_version, CORE_ABI_VERSION);
        assert_eq!(diag.gateway_relay_mode, "Inactive");
        assert_eq!(diag.gateway_observed_path, "Unknown");
        assert_eq!(diag.gateway_path_observed_at_unix, 0);
        assert!(!diag.gateway_relay_disabled);

        assert_eq!(bitchat_detach_ui(), 0);
        let diag2: DiagnosticsSnapshot =
            serde_json::from_str(&take_c_string(bitchat_get_diagnostics()).unwrap()).unwrap();
        assert!(!diag2.ui_attached);

        // 4. Generation-checked callbacks accept current generation & reject stale
        let peer_sid = CString::new("0102030405060708").unwrap();
        assert_eq!(bitchat_on_peer_connected_gen(gen1, peer_sid.as_ptr()), 0);
        assert_eq!(
            bitchat_on_peer_connected_gen(gen1 + 99, peer_sid.as_ptr()),
            -1
        );
        let stale_err = take_c_string(bitchat_last_error()).unwrap();
        assert!(stale_err.contains("STALE_GENERATION"));

        let fake_frame = [1u8, 2, 3, 4];
        assert_eq!(
            bitchat_on_frame_received_gen(gen1 + 5, peer_sid.as_ptr(), fake_frame.as_ptr(), 4),
            -1
        );
        assert!(take_c_string(bitchat_last_error())
            .unwrap()
            .contains("STALE_GENERATION"));

        // Drain outbound auto-handshake bytes & verify bitchat_free_bytes pairing
        let mut out_target: *mut c_char = std::ptr::null_mut();
        let mut out_data: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        while bitchat_poll_outbound(&mut out_target, &mut out_data, &mut out_len) == 1 {
            bitchat_free_string(out_target);
            bitchat_free_bytes(out_data, out_len);
            out_target = std::ptr::null_mut();
            out_data = std::ptr::null_mut();
            out_len = 0;
        }

        // 5. Validation hardening: null, invalid UTF-8, oversized string/frame, zero len, invalid enum, placeholder ShortId
        assert!(bitchat_send_broadcast(std::ptr::null()).is_null());
        assert!(take_c_string(bitchat_last_error())
            .unwrap()
            .contains("FFI_NULL_PTR"));

        let bad_utf8 = [0xffu8, 0xfe, 0x00];
        assert!(bitchat_send_broadcast(bad_utf8.as_ptr().cast::<c_char>()).is_null());
        assert!(take_c_string(bitchat_last_error())
            .unwrap()
            .contains("FFI_INVALID_UTF8"));

        let mut too_long_str = vec![b'x'; MAX_FFI_STRING_LEN + 64];
        too_long_str.push(0);
        assert!(bitchat_send_broadcast(too_long_str.as_ptr().cast::<c_char>()).is_null());
        assert!(take_c_string(bitchat_last_error())
            .unwrap()
            .contains("FFI_STRING_TOO_LONG"));

        assert_eq!(
            bitchat_on_frame_received(peer_sid.as_ptr(), fake_frame.as_ptr(), 0),
            -1
        );
        assert!(take_c_string(bitchat_last_error())
            .unwrap()
            .contains("FFI_FRAME_BOUNDS"));

        let huge_frame = vec![0xAAu8; MAX_FFI_FRAME_LEN + 1];
        assert_eq!(
            bitchat_on_frame_received(peer_sid.as_ptr(), huge_frame.as_ptr(), huge_frame.len()),
            -1
        );
        assert!(take_c_string(bitchat_last_error())
            .unwrap()
            .contains("FFI_FRAME_BOUNDS"));

        assert!(bitchat_send_message(peer_sid.as_ptr(), c"hi".as_ptr(), 99).is_null());
        assert!(take_c_string(bitchat_last_error())
            .unwrap()
            .contains("INVALID_MODE"));

        let zero_placeholder = CString::new("0000000000000000").unwrap();
        assert_eq!(
            bitchat_on_peer_connected_gen(gen1, zero_placeholder.as_ptr()),
            -1
        );

        // Auth/request FFI helpers allocate empty JSON arrays cleanly
        let auth_events = take_c_string(bitchat_poll_auth_events()).unwrap();
        assert_eq!(auth_events, "[]");
        let pending_reqs = take_c_string(bitchat_pending_message_requests()).unwrap();
        assert_eq!(pending_reqs, "[]");

        let mut inbound_json: *mut c_char = std::ptr::null_mut();
        assert_eq!(bitchat_poll_inbound(&mut inbound_json), 0);

        // 6. Double shutdown + stale gen1 rejection + restart increments generation
        assert_eq!(bitchat_shutdown(), 0);
        assert_eq!(bitchat_shutdown(), 0);
        assert_eq!(
            bitchat_on_peer_disconnected_gen(gen1, peer_sid.as_ptr()),
            -1
        );

        // Missing DEK fails closed with typed error!
        assert!(bitchat_init_with_key(c_path.as_ptr(), std::ptr::null(), 1).is_null());
        let key_err = take_c_string(bitchat_last_error()).unwrap();
        assert!(
            key_err.contains("KEY_UNAVAILABLE"),
            "Missing DEK on existing protected storage must fail closed: {key_err}"
        );

        // Restart with DEK preserves identity & increments generation
        let s3 = take_c_string(bitchat_init_with_key(c_path.as_ptr(), dek.as_ptr(), 1))
            .expect("restart with same DEK succeeds");
        assert_eq!(s3, s1);
        assert!(bitchat_runtime_generation() > gen1);

        // Unconfirmed reset refused; confirmed reset cleans up
        assert_eq!(bitchat_destructive_reset_storage(c_path.as_ptr(), 0), -1);
        assert_eq!(bitchat_destructive_reset_storage(c_path.as_ptr(), 1), 0);
        assert!(!dir.join("identity.enc").exists());
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_t5_ffi_concurrent_calls_no_deadlock() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    rt_block(async {
        let _ = api::shutdown().await;
    });

    let dir = fresh_temp_dir("t5_ffi_conc");
    let c_path = CString::new(dir.to_str().unwrap()).unwrap();
    let dek = [0x99u8; 32];

    unsafe {
        assert!(!bitchat_init_with_key(c_path.as_ptr(), dek.as_ptr(), 1).is_null());
    }

    let mut handles = Vec::new();
    for worker in 0..4 {
        handles.push(std::thread::spawn(move || {
            for iter in 0..10 {
                let msg = CString::new(format!("concurrent-{worker}-{iter}")).unwrap();
                unsafe {
                    let id = take_c_string(bitchat_send_broadcast(msg.as_ptr()));
                    assert!(id.is_some());
                    let diag = take_c_string(bitchat_get_diagnostics());
                    assert!(diag.is_some());
                }
            }
        }));
    }

    for h in handles {
        h.join().expect("worker thread must not panic");
    }

    unsafe {
        assert_eq!(bitchat_shutdown(), 0);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_t5_live_iroh_path_observation_and_gateway_diagnostics() {
    let dir_a = fresh_temp_dir("t5_gw_a");
    let dir_b = fresh_temp_dir("t5_gw_b");
    let kp_a: std::sync::Arc<dyn KeyProvider> =
        std::sync::Arc::new(InjectedKeyProvider::new([0x11u8; 32], 1));
    let kp_b: std::sync::Arc<dyn KeyProvider> =
        std::sync::Arc::new(InjectedKeyProvider::new([0x22u8; 32], 1));

    let mut svc_a =
        api::BitchatCoreService::new_with_key_provider(dir_a.to_string_lossy().into_owned(), kp_a)
            .await
            .unwrap();
    let mut svc_b =
        api::BitchatCoreService::new_with_key_provider(dir_b.to_string_lossy().into_owned(), kp_b)
            .await
            .unwrap();

    // Before gateway start: Inactive + Unknown + 0 timestamp
    let diag0 = svc_a.get_diagnostics_snapshot().await;
    assert_eq!(diag0.gateway_relay_mode, "Inactive");
    assert_eq!(diag0.gateway_observed_path, "Unknown");
    assert_eq!(diag0.gateway_path_observed_at_unix, 0);
    assert!(!diag0.gateway_relay_disabled);

    // Start with disabled relays, before any active session -> relay mode Disabled, path still Unknown
    svc_a
        .gateway_start(Some("disabled".to_string()))
        .await
        .unwrap();
    svc_b
        .gateway_start(Some("disabled".to_string()))
        .await
        .unwrap();

    let diag1 = svc_a.get_diagnostics_snapshot().await;
    assert!(diag1.gateway_active);
    assert!(diag1.gateway_relay_disabled);
    assert_eq!(diag1.gateway_relay_mode, "Disabled");
    assert_eq!(diag1.gateway_observed_path, "Unknown");

    // Establish real loopback Iroh gateway session between A and B
    let contact_b = svc_b.gateway_export_contact().await.unwrap();
    svc_a
        .gateway_import_and_approve_contact(contact_b.clone())
        .await
        .unwrap();
    svc_a.gateway_connect_to_contact(contact_b).await.unwrap();

    // Give path watcher a brief moment to reflect the open loopback IP path
    let mut observed_direct = false;
    let mut diag2 = svc_a.get_diagnostics_snapshot().await;
    for _ in 0..20 {
        diag2 = svc_a.get_diagnostics_snapshot().await;
        if diag2.gateway_observed_path == "Direct" {
            observed_direct = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        observed_direct,
        "Expected live loopback Iroh session to report Direct path, got: {} (ts={})",
        diag2.gateway_observed_path, diag2.gateway_path_observed_at_unix
    );
    assert!(diag2.gateway_path_observed_at_unix > 0);

    let _ = svc_a.gateway_stop().await;
    let _ = svc_b.gateway_stop().await;
    drop(svc_a);
    drop(svc_b);
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

#[test]
fn test_t5_ffi_gateway_start_stop() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    rt_block(async {
        let _ = api::shutdown().await;
    });
    let dir_c = fresh_temp_dir("t5_gw_c");
    let c_path = CString::new(dir_c.to_str().unwrap()).unwrap();
    unsafe {
        assert!(!bitchat_init_with_key(c_path.as_ptr(), [0x33u8; 32].as_ptr(), 1).is_null());
        let ep = take_c_string(bitchat_gateway_start(c"disabled".as_ptr())).expect("gw start");
        assert!(!ep.is_empty());
        assert_eq!(bitchat_gateway_stop(), 0);
        assert_eq!(bitchat_shutdown(), 0);
    }
    let _ = std::fs::remove_dir_all(&dir_c);
}
