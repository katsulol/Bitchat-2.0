//! C-ABI FFI surface for `bitchat2_core`.
//!
//! # Safety, Validation & Panic-Boundary Guarantees
//! 1. **Bounded input validation**: All `*const c_char` strings are scanned up to
//!    [`MAX_FFI_STRING_LEN`] (64 KiB) for a NUL terminator and strictly validated as UTF-8
//!    before allocation. All raw frame buffers are checked non-null, non-empty, and `<= MAX_FFI_FRAME_LEN`.
//! 2. **Panic boundary (`catch_unwind`)**: Every `extern "C"` export runs inside
//!    `std::panic::catch_unwind(std::panic::AssertUnwindSafe(...))` so Rust never unwinds across
//!    the C ABI (which would be Undefined Behavior). Catching panics is defense-in-depth and does
//!    not replace input validation; release builds configured with `panic = "abort"` still abort cleanly
//!    instead of unwinding.
//! 3. **Typed error reporting**: Validation errors, runtime errors, ABI mismatches, and caught panics
//!    record diagnostic messages via `api::record_last_error` readable through [`bitchat_last_error`].
//! 4. **Ownership / free pairing**: Every non-null `*mut c_char` returned directly or via an out-pointer
//!    must be freed exactly once via [`bitchat_free_string`]; outbound byte buffers returned by
//!    [`bitchat_poll_outbound`] must be freed via [`bitchat_free_bytes`].

use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::OnceLock;
use tokio::runtime::Runtime;
use zeroize::Zeroize;

use crate::api;

/// Maximum UTF-8 byte length accepted for C-string FFI arguments (excluding trailing NUL).
pub const MAX_FFI_STRING_LEN: usize = 64 * 1024;
/// Maximum raw wire frame length accepted over FFI.
pub const MAX_FFI_FRAME_LEN: usize = 64 * 1024;

static RUNTIME: OnceLock<Option<Runtime>> = OnceLock::new();

fn get_rt() -> Option<&'static Runtime> {
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .ok()
        })
        .as_ref()
}

fn require_rt() -> Result<&'static Runtime, String> {
    get_rt().ok_or_else(|| "RUNTIME_UNAVAILABLE: Failed to initialize Tokio runtime".to_string())
}

/// # Safety
///
/// If `ptr` is non-null, it must point to a valid NUL-terminated buffer readable up to
/// `max_len + 1` bytes.
unsafe fn c_str_to_string_bounded(ptr: *const c_char, max_len: usize) -> Result<String, String> {
    if ptr.is_null() {
        return Err("FFI_NULL_PTR: Null C string pointer".to_string());
    }
    let byte_ptr = ptr.cast::<u8>();
    let mut len = 0usize;
    while len <= max_len {
        if *byte_ptr.add(len) == 0 {
            let bytes = std::slice::from_raw_parts(byte_ptr, len);
            return std::str::from_utf8(bytes)
                .map(|s| s.to_string())
                .map_err(|e| format!("FFI_INVALID_UTF8: {e}"));
        }
        len += 1;
    }
    Err(format!(
        "FFI_STRING_TOO_LONG: Input exceeds maximum {max_len} bytes"
    ))
}

/// # Safety
///
/// Accepts null (`Ok(None)`) or a valid bounded NUL-terminated UTF-8 C string (`Ok(Some(_))`).
unsafe fn c_str_opt_bounded(ptr: *const c_char, max_len: usize) -> Result<Option<String>, String> {
    if ptr.is_null() {
        Ok(None)
    } else {
        c_str_to_string_bounded(ptr, max_len).map(Some)
    }
}

fn string_to_c_char(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => {
            api::record_last_error("FFI_INTERIOR_NUL: Output string contained interior NUL");
            std::ptr::null_mut()
        }
    }
}

fn record_and_null(err: impl Into<String>) -> *mut c_char {
    api::record_last_error(err);
    std::ptr::null_mut()
}

fn record_and_err_code(err: impl Into<String>) -> i32 {
    api::record_last_error(err);
    -1
}

fn ffi_guard_ptr<F: FnOnce() -> *mut c_char>(f: F) -> *mut c_char {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            api::record_last_error("FFI_PANIC: Unwind caught at C-ABI boundary");
            std::ptr::null_mut()
        }
    }
}

fn ffi_guard_i32<F: FnOnce() -> i32>(f: F) -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            api::record_last_error("FFI_PANIC: Unwind caught at C-ABI boundary");
            -1
        }
    }
}

fn ffi_guard_u32<F: FnOnce() -> u32>(f: F) -> u32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            api::record_last_error("FFI_PANIC: Unwind caught at C-ABI boundary");
            0
        }
    }
}

fn ffi_guard_u64<F: FnOnce() -> u64>(f: F) -> u64 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            api::record_last_error("FFI_PANIC: Unwind caught at C-ABI boundary");
            0
        }
    }
}

fn ffi_guard_void<F: FnOnce()>(f: F) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
        api::record_last_error("FFI_PANIC: Unwind caught in destructor");
    }
}

/// Return the compiled C-ABI version integer (`CORE_ABI_VERSION`).
///
/// # Safety
///
/// Safe to call before or after [`bitchat_init`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_abi_version() -> u32 {
    ffi_guard_u32(|| api::CORE_ABI_VERSION)
}

/// Verify caller-expected C-ABI version matches native library (`0` = compatible, `-1` = mismatch).
///
/// # Safety
///
/// On mismatch, records an `"ABI_MISMATCH: ..."` reason retrievable via [`bitchat_last_error`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_check_abi_version(expected_abi: u32) -> i32 {
    ffi_guard_i32(|| match api::check_abi_compatibility(expected_abi) {
        Ok(_) => 0,
        Err(e) => record_and_err_code(e),
    })
}

/// Return JSON metadata describing native build version, ABI version, git commit, dirty state, and target.
///
/// # Safety
///
/// Returned pointer must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_build_info() -> *mut c_char {
    ffi_guard_ptr(|| string_to_c_char(api::build_info_json()))
}

/// Return the most recent error message recorded across core/FFI calls, or null if none.
///
/// # Safety
///
/// Returned pointer (if non-null) must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_last_error() -> *mut c_char {
    ffi_guard_ptr(|| match api::last_error() {
        Some(err) => string_to_c_char(err),
        None => std::ptr::null_mut(),
    })
}

/// Return the active monotonic runtime lifecycle generation token (`0` when never initialized).
///
/// # Safety
///
/// Safe to call from any thread.
#[no_mangle]
pub unsafe extern "C" fn bitchat_runtime_generation() -> u64 {
    ffi_guard_u64(api::runtime_generation)
}

/// Attach UI lifecycle observer and return the current runtime lifecycle generation token.
///
/// # Safety
///
/// Safe to call repeatedly (idempotent).
#[no_mangle]
pub unsafe extern "C" fn bitchat_attach_ui() -> u64 {
    ffi_guard_u64(api::attach_ui)
}

/// Detach UI lifecycle observer without terminating core runtime ownership.
///
/// # Safety
///
/// Returns `0` on success.
#[no_mangle]
pub unsafe extern "C" fn bitchat_detach_ui() -> i32 {
    ffi_guard_i32(|| {
        api::detach_ui();
        0
    })
}

/// Gracefully shut down the core runtime service, stop gateway bridge, and invalidate generation tokens.
///
/// # Safety
///
/// Idempotent; returns `0` on success or `-1` on failure.
#[no_mangle]
pub unsafe extern "C" fn bitchat_shutdown() -> i32 {
    ffi_guard_i32(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::shutdown()) {
            Ok(()) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Explicitly wipe encrypted databases and root identity ONLY when `confirm != 0`.
///
/// # Safety
///
/// `storage_path` must be a valid NUL-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn bitchat_destructive_reset_storage(
    storage_path: *const c_char,
    confirm: u8,
) -> i32 {
    ffi_guard_i32(|| {
        let path = match c_str_to_string_bounded(storage_path, MAX_FFI_STRING_LEN) {
            Ok(p) => p,
            Err(e) => return record_and_err_code(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::destructive_reset_storage(path, confirm != 0)) {
            Ok(()) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Initialize Bitchat core with persistent identity and keystore.
///
/// # Safety
///
/// Caller must provide a valid null-terminated C string for `storage_path`.
/// The returned pointer must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_init(storage_path: *const c_char) -> *mut c_char {
    ffi_guard_ptr(|| {
        let path = match c_str_to_string_bounded(storage_path, MAX_FFI_STRING_LEN) {
            Ok(p) => p,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::init(path)) {
            Ok(short_id) => string_to_c_char(short_id),
            Err(e) => record_and_null(e),
        }
    })
}

/// Initialize Bitchat core with hardware-backed DEK key from Android Keystore.
///
/// # Safety
///
/// `storage_path` must be a valid null-terminated C string.
/// `key_ptr` must point to a 32-byte buffer, or be null.
#[no_mangle]
pub unsafe extern "C" fn bitchat_init_with_key(
    storage_path: *const c_char,
    key_ptr: *const u8,
    version: u32,
) -> *mut c_char {
    ffi_guard_ptr(|| {
        let path = match c_str_to_string_bounded(storage_path, MAX_FFI_STRING_LEN) {
            Ok(p) => p,
            Err(e) => return record_and_null(e),
        };

        let mut key_buf = [0u8; 32];
        let dek = if !key_ptr.is_null() {
            std::ptr::copy_nonoverlapping(key_ptr, key_buf.as_mut_ptr(), 32);
            Some(key_buf)
        } else {
            None
        };

        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => {
                key_buf.zeroize();
                return record_and_null(e);
            }
        };

        let res = rt.block_on(api::init_with_key(path, dek, version));
        key_buf.zeroize();
        match res {
            Ok(short_id) => string_to_c_char(short_id),
            Err(e) => record_and_null(e),
        }
    })
}

/// Return own 8-byte hexadecimal ShortId.
///
/// # Safety
///
/// Returned pointer must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_my_short_id() -> *mut c_char {
    ffi_guard_ptr(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::my_short_id()) {
            Ok(id) => string_to_c_char(id),
            Err(e) => record_and_null(e),
        }
    })
}

/// Return own full 32-byte hexadecimal public key.
///
/// # Safety
///
/// Returned pointer must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_my_peer_id() -> *mut c_char {
    ffi_guard_ptr(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::my_peer_id()) {
            Ok(pk) => string_to_c_char(pk),
            Err(e) => record_and_null(e),
        }
    })
}

/// Broadcast a message to all mesh peers.
///
/// # Safety
///
/// `text` must be a valid null-terminated C string. Returned pointer must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_send_broadcast(text: *const c_char) -> *mut c_char {
    ffi_guard_ptr(|| {
        let body = match c_str_to_string_bounded(text, MAX_FFI_STRING_LEN) {
            Ok(b) => b,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::send_broadcast(body)) {
            Ok(msg_id) => string_to_c_char(msg_id),
            Err(e) => record_and_null(e),
        }
    })
}

/// Send an encrypted direct message to a specific recipient ShortId.
///
/// # Safety
///
/// `recipient` and `text` must be valid null-terminated C strings. Returned pointer must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_send_to(
    recipient: *const c_char,
    text: *const c_char,
) -> *mut c_char {
    ffi_guard_ptr(|| {
        let target = match c_str_to_string_bounded(recipient, MAX_FFI_STRING_LEN) {
            Ok(t) => t,
            Err(e) => return record_and_null(e),
        };
        let body = match c_str_to_string_bounded(text, MAX_FFI_STRING_LEN) {
            Ok(b) => b,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::send_to(target, body)) {
            Ok(msg_id) => string_to_c_char(msg_id),
            Err(e) => record_and_null(e),
        }
    })
}

/// Explicitly initiate a Noise XX handshake with a peer across mesh or gateway.
///
/// # Safety
///
/// `recipient` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn bitchat_initiate_handshake(recipient: *const c_char) -> i32 {
    ffi_guard_i32(|| {
        let target = match c_str_to_string_bounded(recipient, MAX_FFI_STRING_LEN) {
            Ok(t) => t,
            Err(e) => return record_and_err_code(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::initiate_handshake(target)) {
            Ok(_) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Check if an authenticated Noise XX E2E session is established with `recipient`.
///
/// Returns 1 if session is ready for transport, 0 otherwise.
///
/// # Safety
///
/// `recipient` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn bitchat_is_session_ready(recipient: *const c_char) -> i32 {
    ffi_guard_i32(|| {
        let target = match c_str_to_string_bounded(recipient, MAX_FFI_STRING_LEN) {
            Ok(t) => t,
            Err(e) => {
                api::record_last_error(e);
                return 0;
            }
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => {
                api::record_last_error(e);
                return 0;
            }
        };
        match rt.block_on(api::is_session_ready(target)) {
            Ok(true) => 1,
            Ok(false) => 0,
            Err(e) => {
                api::record_last_error(e);
                0
            }
        }
    })
}

/// Notify mesh of peer connection.
///
/// # Safety
///
/// `short_id` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn bitchat_on_peer_connected(short_id: *const c_char) -> i32 {
    ffi_guard_i32(|| {
        let peer = match c_str_to_string_bounded(short_id, MAX_FFI_STRING_LEN) {
            Ok(p) => p,
            Err(e) => return record_and_err_code(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::on_peer_connected(peer)) {
            Ok(_) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Generation-checked variant of [`bitchat_on_peer_connected`].
///
/// # Safety
///
/// `short_id` must be a valid null-terminated C string. Drops stale generation callbacks cleanly.
#[no_mangle]
pub unsafe extern "C" fn bitchat_on_peer_connected_gen(
    generation: u64,
    short_id: *const c_char,
) -> i32 {
    ffi_guard_i32(|| {
        let peer = match c_str_to_string_bounded(short_id, MAX_FFI_STRING_LEN) {
            Ok(p) => p,
            Err(e) => return record_and_err_code(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::on_peer_connected_gen(generation, peer)) {
            Ok(_) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Notify mesh of peer disconnection.
///
/// # Safety
///
/// `short_id` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn bitchat_on_peer_disconnected(short_id: *const c_char) -> i32 {
    ffi_guard_i32(|| {
        let peer = match c_str_to_string_bounded(short_id, MAX_FFI_STRING_LEN) {
            Ok(p) => p,
            Err(e) => return record_and_err_code(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::on_peer_disconnected(peer)) {
            Ok(_) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Generation-checked variant of [`bitchat_on_peer_disconnected`].
///
/// # Safety
///
/// `short_id` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn bitchat_on_peer_disconnected_gen(
    generation: u64,
    short_id: *const c_char,
) -> i32 {
    ffi_guard_i32(|| {
        let peer = match c_str_to_string_bounded(short_id, MAX_FFI_STRING_LEN) {
            Ok(p) => p,
            Err(e) => return record_and_err_code(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::on_peer_disconnected_gen(generation, peer)) {
            Ok(_) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Feed an incoming raw wire frame into the mesh.
///
/// # Safety
///
/// `short_id` must be null-terminated. `data` must point to a buffer of `len` bytes (`1..=MAX_FFI_FRAME_LEN`).
#[no_mangle]
pub unsafe extern "C" fn bitchat_on_frame_received(
    short_id: *const c_char,
    data: *const u8,
    len: usize,
) -> i32 {
    ffi_guard_i32(|| {
        let peer = match c_str_to_string_bounded(short_id, MAX_FFI_STRING_LEN) {
            Ok(p) => p,
            Err(e) => return record_and_err_code(e),
        };
        if data.is_null() || len == 0 || len > MAX_FFI_FRAME_LEN {
            return record_and_err_code(format!(
                "FFI_FRAME_BOUNDS: Invalid frame pointer/len ({len}, max {MAX_FFI_FRAME_LEN})"
            ));
        }
        let bytes = std::slice::from_raw_parts(data, len).to_vec();
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::on_frame_received(peer, bytes)) {
            Ok(_) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Generation-checked variant of [`bitchat_on_frame_received`].
///
/// # Safety
///
/// `short_id` must be null-terminated. `data` must point to a buffer of `len` bytes (`1..=MAX_FFI_FRAME_LEN`).
#[no_mangle]
pub unsafe extern "C" fn bitchat_on_frame_received_gen(
    generation: u64,
    short_id: *const c_char,
    data: *const u8,
    len: usize,
) -> i32 {
    ffi_guard_i32(|| {
        let peer = match c_str_to_string_bounded(short_id, MAX_FFI_STRING_LEN) {
            Ok(p) => p,
            Err(e) => return record_and_err_code(e),
        };
        if data.is_null() || len == 0 || len > MAX_FFI_FRAME_LEN {
            return record_and_err_code(format!(
                "FFI_FRAME_BOUNDS: Invalid frame pointer/len ({len}, max {MAX_FFI_FRAME_LEN})"
            ));
        }
        let bytes = std::slice::from_raw_parts(data, len).to_vec();
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::on_frame_received_gen(generation, peer, bytes)) {
            Ok(_) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Poll outbound wire frame for BLE transmission.
///
/// # Safety
///
/// `out_target`, `out_data`, and `out_len` must be valid, non-null writable pointers.
/// Allocated buffers must be freed using [`bitchat_free_string`] and [`bitchat_free_bytes`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_poll_outbound(
    out_target: *mut *mut c_char,
    out_data: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    ffi_guard_i32(|| {
        if out_target.is_null() || out_data.is_null() || out_len.is_null() {
            return record_and_err_code("FFI_NULL_PTR: Null output pointer in poll_outbound");
        }
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::poll_outbound()) {
            Ok(Some(frame)) => {
                *out_target = match frame.to {
                    Some(to) => string_to_c_char(to),
                    None => std::ptr::null_mut(),
                };
                let len = frame.bytes.len();
                *out_len = len;
                if len == 0 {
                    *out_data = std::ptr::null_mut();
                } else {
                    let mut boxed = frame.bytes.into_boxed_slice();
                    let raw_ptr = boxed.as_mut_ptr();
                    std::mem::forget(boxed);
                    *out_data = raw_ptr;
                }
                1
            }
            Ok(None) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Poll received inbound message for UI presentation.
///
/// # Safety
///
/// `out_json` must be a valid, non-null pointer to a `*mut c_char`.
/// Returned JSON string must be freed using [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_poll_inbound(out_json: *mut *mut c_char) -> i32 {
    ffi_guard_i32(|| {
        if out_json.is_null() {
            return record_and_err_code("FFI_NULL_PTR: Null out_json pointer");
        }
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::poll_inbound()) {
            Ok(Some(msg)) => {
                let json = serde_json::json!({
                    "msg_id": msg.msg_id,
                    "sender": msg.sender,
                    "recipient": msg.recipient,
                    "text": msg.text,
                    "hop_count": msg.hop_count,
                    "is_broadcast": msg.is_broadcast,
                    "auth_status": msg.auth_status,
                })
                .to_string();
                let ptr = string_to_c_char(json);
                *out_json = ptr;
                if ptr.is_null() {
                    -1
                } else {
                    1
                }
            }
            Ok(None) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Start internet gateway bridge on local node.
///
/// # Safety
///
/// `relay_mode` may be null or a null-terminated C string ("disabled" or "default").
/// Returned endpoint ID string must be freed using [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_gateway_start(relay_mode: *const c_char) -> *mut c_char {
    ffi_guard_ptr(|| {
        let mode = match c_str_opt_bounded(relay_mode, MAX_FFI_STRING_LEN) {
            Ok(m) => m,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::gateway_start(mode)) {
            Ok(endpoint_id) => string_to_c_char(endpoint_id),
            Err(e) => record_and_null(e),
        }
    })
}

/// Stop internet gateway bridge on local node.
///
/// # Safety
///
/// Returns 0 on success, -1 on failure.
#[no_mangle]
pub unsafe extern "C" fn bitchat_gateway_stop() -> i32 {
    ffi_guard_i32(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::gateway_stop()) {
            Ok(_) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Export this node's gateway contact string (URI).
///
/// # Safety
///
/// Returned URI string must be freed using [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_gateway_export_contact() -> *mut c_char {
    ffi_guard_ptr(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::gateway_export_contact()) {
            Ok(uri) => string_to_c_char(uri),
            Err(e) => record_and_null(e),
        }
    })
}

/// Import, verify, and approve a remote gateway contact string.
///
/// # Safety
///
/// `contact_str` must be a valid null-terminated C string.
/// Returned endpoint public key hex must be freed using [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_gateway_import_and_approve_contact(
    contact_str: *const c_char,
) -> *mut c_char {
    ffi_guard_ptr(|| {
        let contact = match c_str_to_string_bounded(contact_str, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::gateway_import_and_approve_contact(contact)) {
            Ok(pk) => string_to_c_char(pk),
            Err(e) => record_and_null(e),
        }
    })
}

/// Dial and establish connection to an approved remote gateway contact URI.
///
/// # Safety
///
/// `contact_str` must be a valid null-terminated C string.
/// Returns 0 on success, -1 on failure.
#[no_mangle]
pub unsafe extern "C" fn bitchat_gateway_connect_to_contact(contact_str: *const c_char) -> i32 {
    ffi_guard_i32(|| {
        let contact = match c_str_to_string_bounded(contact_str, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_err_code(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::gateway_connect_to_contact(contact)) {
            Ok(_) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Set manual route mapping a destination ShortId hex to a target Gateway EndpointId hex.
///
/// # Safety
///
/// `recipient_short_id` and `target_endpoint_id` must be valid null-terminated C strings.
/// Returns 0 on success, -1 on failure.
#[no_mangle]
pub unsafe extern "C" fn bitchat_gateway_set_route(
    recipient_short_id: *const c_char,
    target_endpoint_id: *const c_char,
) -> i32 {
    ffi_guard_i32(|| {
        let recipient = match c_str_to_string_bounded(recipient_short_id, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_err_code(e),
        };
        let target = match c_str_to_string_bounded(target_endpoint_id, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_err_code(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::gateway_set_route(recipient, target)) {
            Ok(_) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Get current gateway statistics snapshot as JSON string.
///
/// # Safety
///
/// Returned JSON string must be freed using [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_gateway_get_stats() -> *mut c_char {
    ffi_guard_ptr(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::gateway_get_stats()) {
            Ok(json) => string_to_c_char(json),
            Err(e) => record_and_null(e),
        }
    })
}

/// Query comma-separated list of remote recipient ShortIds currently reachable through gateways.
///
/// # Safety
///
/// Returned comma-separated string must be freed using [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_gateway_get_routes() -> *mut c_char {
    ffi_guard_ptr(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::gateway_get_routes()) {
            Ok(routes) => string_to_c_char(routes.join(",")),
            Err(e) => record_and_null(e),
        }
    })
}

/// Send an end-to-end encrypted direct chat message with delivery tracking and outbox admission.
///
/// # Safety
///
/// `recipient` and `text` must be valid null-terminated C strings.
/// Returned logical ID string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_send_message(
    recipient: *const c_char,
    text: *const c_char,
    mode: u8,
) -> *mut c_char {
    ffi_guard_ptr(|| {
        if mode > 1 {
            return record_and_null(format!(
                "INVALID_MODE: unsupported mode {mode}; expected 0 (Normal) or 1 (Privacy)"
            ));
        }
        let target = match c_str_to_string_bounded(recipient, MAX_FFI_STRING_LEN) {
            Ok(t) => t,
            Err(e) => return record_and_null(e),
        };
        let body = match c_str_to_string_bounded(text, MAX_FFI_STRING_LEN) {
            Ok(b) => b,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::send_message(target, body, mode)) {
            Ok(logical_id) => string_to_c_char(logical_id),
            Err(e) => record_and_null(e),
        }
    })
}

/// Send an end-to-end encrypted direct chat message with an explicit optional deadline (seconds).
///
/// Pass a negative `deadline_secs` (e.g., `-1`) to use the mode's default deadline;
/// non-negative values (`>= 0`) are validated strictly against mode bounds (`MIN_DEADLINE_SECS..=mode.max_ttl()`).
///
/// # Safety
///
/// `recipient` and `text` must be valid null-terminated C strings.
/// Returned logical ID string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_send_message_with_deadline(
    recipient: *const c_char,
    text: *const c_char,
    mode: u8,
    deadline_secs: i64,
) -> *mut c_char {
    ffi_guard_ptr(|| {
        if mode > 1 {
            return record_and_null(format!(
                "INVALID_MODE: unsupported mode {mode}; expected 0 (Normal) or 1 (Privacy)"
            ));
        }
        let target = match c_str_to_string_bounded(recipient, MAX_FFI_STRING_LEN) {
            Ok(t) => t,
            Err(e) => return record_and_null(e),
        };
        let body = match c_str_to_string_bounded(text, MAX_FFI_STRING_LEN) {
            Ok(b) => b,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        let deadline_opt = if deadline_secs < 0 {
            None
        } else {
            Some(deadline_secs as u64)
        };
        match rt.block_on(api::send_message_with_deadline(
            target,
            body,
            mode,
            deadline_opt,
        )) {
            Ok(logical_id) => string_to_c_char(logical_id),
            Err(e) => record_and_null(e),
        }
    })
}

/// Cancel a non-terminal outbound logical message by its 32-character hex ID.
///
/// Returns `1` if cancelled, `0` if already terminal or not found, or `-1` on invalid input/error.
///
/// # Safety
///
/// `logical_id` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn bitchat_cancel_message(logical_id: *const c_char) -> i32 {
    ffi_guard_i32(|| {
        let id = match c_str_to_string_bounded(logical_id, MAX_FFI_STRING_LEN) {
            Ok(i) => i,
            Err(e) => return record_and_err_code(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::cancel_message(id)) {
            Ok(true) => 1,
            Ok(false) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Query delivery state of a logical message by its 32-character hex ID.
///
/// # Safety
///
/// `logical_id` must be a valid null-terminated C string.
/// Returned state string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_get_delivery_state(logical_id: *const c_char) -> *mut c_char {
    ffi_guard_ptr(|| {
        let id = match c_str_to_string_bounded(logical_id, MAX_FFI_STRING_LEN) {
            Ok(i) => i,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::get_delivery_state(id)) {
            Ok(state) => string_to_c_char(state),
            Err(e) => record_and_null(e),
        }
    })
}

/// Drain queued delivery transition events emitted by the delivery subsystem.
///
/// Returns JSON array of delivery events.
///
/// # Safety
///
/// Returned JSON string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_poll_delivery_events() -> *mut c_char {
    ffi_guard_ptr(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::poll_delivery_events()) {
            Ok(json) => string_to_c_char(json),
            Err(e) => record_and_null(e),
        }
    })
}

/// Drain queued inbound authorization events as JSON array.
///
/// # Safety
///
/// Returned JSON string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_poll_auth_events() -> *mut c_char {
    ffi_guard_ptr(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::poll_auth_events_json()) {
            Ok(json) => string_to_c_char(json),
            Err(e) => record_and_null(e),
        }
    })
}

/// Query queued inbound pending message requests as JSON array.
///
/// # Safety
///
/// Returned JSON string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_pending_message_requests() -> *mut c_char {
    ffi_guard_ptr(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::pending_message_requests_json()) {
            Ok(json) => string_to_c_char(json),
            Err(e) => record_and_null(e),
        }
    })
}

/// Retrieve comprehensive routing diagnostics and quota snapshot for mobile UI.
///
/// Returns JSON object of diagnostics snapshot.
///
/// # Safety
///
/// Returned JSON string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_get_diagnostics() -> *mut c_char {
    ffi_guard_ptr(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::get_diagnostics()) {
            Ok(json) => string_to_c_char(json),
            Err(e) => record_and_null(e),
        }
    })
}

/// Step the outbox pump, processing timeouts, retries, and expiration.
///
/// Returns number of handled delivery actions, or -1 on error.
///
/// # Safety
///
/// This function is safe to call across thread boundaries.
#[no_mangle]
pub unsafe extern "C" fn bitchat_pump_outbox() -> i32 {
    ffi_guard_i32(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::pump_outbox()) {
            Ok(count) => i32::try_from(count).unwrap_or(i32::MAX),
            Err(e) => record_and_err_code(e),
        }
    })
}

/// List incoming gateway peers currently in `IdentityVerifiedPendingConsent` as a JSON array.
///
/// # Safety
///
/// Returned JSON string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_gateway_list_pending() -> *mut c_char {
    ffi_guard_ptr(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::gateway_list_pending()) {
            Ok(json) => string_to_c_char(json),
            Err(e) => record_and_null(e),
        }
    })
}

/// Explicitly approve a pending or recognized gateway endpoint ID.
///
/// # Safety
///
/// `endpoint_id` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn bitchat_gateway_approve_peer(endpoint_id: *const c_char) -> i32 {
    ffi_guard_i32(|| {
        let ep = match c_str_to_string_bounded(endpoint_id, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_err_code(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::gateway_approve_peer(ep)) {
            Ok(_) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Revoke (`block == 0`) or permanently block (`block != 0`) a gateway endpoint ID.
///
/// # Safety
///
/// `endpoint_id` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn bitchat_gateway_revoke_or_block_peer(
    endpoint_id: *const c_char,
    block: u8,
) -> i32 {
    ffi_guard_i32(|| {
        let ep = match c_str_to_string_bounded(endpoint_id, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_err_code(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::gateway_revoke_or_block_peer(ep, block != 0)) {
            Ok(_) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Set local trust state for a 64-hex `PeerId` (`"approve"`, `"introducer"`, `"revoke"`, `"block"`).
///
/// # Safety
///
/// `peer_id` and `action` must be valid null-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn bitchat_trust_set_peer_state(
    peer_id: *const c_char,
    action: *const c_char,
) -> i32 {
    ffi_guard_i32(|| {
        let pid = match c_str_to_string_bounded(peer_id, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_err_code(e),
        };
        let act = match c_str_to_string_bounded(action, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_err_code(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_err_code(e),
        };
        match rt.block_on(api::trust_set_peer_state(pid, act)) {
            Ok(_) => 0,
            Err(e) => record_and_err_code(e),
        }
    })
}

/// Issue a signed one-hop Ed25519 capability voucher (`bitchat-voucher://<hex>`).
///
/// # Safety
///
/// `subject`, `audience`, and `capability` must be valid null-terminated C strings.
/// Returned string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_trust_issue_voucher(
    subject: *const c_char,
    audience: *const c_char,
    capability: *const c_char,
    lifetime_secs: u64,
) -> *mut c_char {
    ffi_guard_ptr(|| {
        let subj = match c_str_to_string_bounded(subject, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_null(e),
        };
        let aud = match c_str_to_string_bounded(audience, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_null(e),
        };
        let cap = match c_str_to_string_bounded(capability, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::trust_issue_voucher(subj, aud, cap, lifetime_secs)) {
            Ok(uri) => string_to_c_char(uri),
            Err(e) => record_and_null(e),
        }
    })
}

/// Import and verify a signed one-hop capability voucher (`bitchat-voucher://<hex>`).
///
/// # Safety
///
/// `voucher_uri` must be a valid null-terminated C string.
/// Returned JSON summary string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_trust_import_voucher(voucher_uri: *const c_char) -> *mut c_char {
    ffi_guard_ptr(|| {
        let uri = match c_str_to_string_bounded(voucher_uri, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::trust_import_voucher(uri)) {
            Ok(json) => string_to_c_char(json),
            Err(e) => record_and_null(e),
        }
    })
}

/// Update local device power/resource state and recompute `RelayResourceProfile`.
///
/// # Safety
///
/// Returned profile string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_update_resource_state(
    battery_pct: i32,
    charging: i32,
    power_save: i32,
    thermal: i32,
    user_paused: u8,
) -> *mut c_char {
    ffi_guard_ptr(|| {
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::update_resource_state(
            battery_pct,
            charging,
            power_save,
            thermal,
            user_paused != 0,
        )) {
            Ok(prof) => string_to_c_char(prof),
            Err(e) => record_and_null(e),
        }
    })
}

/// Set user relay contribution profile (`"receive_only"`, `"balanced"`, `"help_network"`, or `"auto"`).
///
/// # Safety
///
/// `mode` must be a valid null-terminated C string.
/// Returned effective profile string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_set_relay_contribution_profile(
    mode: *const c_char,
) -> *mut c_char {
    ffi_guard_ptr(|| {
        let mode_str = match c_str_to_string_bounded(mode, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::set_relay_contribution_profile(mode_str)) {
            Ok(prof) => string_to_c_char(prof),
            Err(e) => record_and_null(e),
        }
    })
}

/// Record a direct-link BLE RSSI sample and return its coarse proximity category.
///
/// # Safety
///
/// `short_id` must be a valid null-terminated C string.
/// Returned category string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_record_ble_rssi(
    short_id: *const c_char,
    rssi_dbm: i8,
) -> *mut c_char {
    ffi_guard_ptr(|| {
        let sid = match c_str_to_string_bounded(short_id, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::record_ble_rssi(sid, rssi_dbm)) {
            Ok(cat) => string_to_c_char(cat),
            Err(e) => record_and_null(e),
        }
    })
}

/// Query coarse proximity category (`"Nearby"`, `"Further"`, `"Unknown"`) for `short_id`.
///
/// # Safety
///
/// `short_id` must be a valid null-terminated C string.
/// Returned category string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_get_peer_proximity(short_id: *const c_char) -> *mut c_char {
    ffi_guard_ptr(|| {
        let sid = match c_str_to_string_bounded(short_id, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::get_peer_proximity(sid)) {
            Ok(cat) => string_to_c_char(cat),
            Err(e) => record_and_null(e),
        }
    })
}

/// Initiate or close an authenticated volatile conversation with `recipient`.
///
/// # Safety
///
/// `recipient` must be a valid null-terminated C string.
/// Returned state string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_set_volatile_session(
    recipient: *const c_char,
    retention_secs: u32,
) -> *mut c_char {
    ffi_guard_ptr(|| {
        let recip = match c_str_to_string_bounded(recipient, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::set_volatile_session(recip, retention_secs)) {
            Ok(st) => string_to_c_char(st),
            Err(e) => record_and_null(e),
        }
    })
}

/// Query volatile conversation state for `recipient` as JSON (`{state, retention_secs}`).
///
/// # Safety
///
/// `recipient` must be a valid null-terminated C string.
/// Returned JSON string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_get_volatile_state(recipient: *const c_char) -> *mut c_char {
    ffi_guard_ptr(|| {
        let recip = match c_str_to_string_bounded(recipient, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::get_volatile_state(recip)) {
            Ok(json) => string_to_c_char(json),
            Err(e) => record_and_null(e),
        }
    })
}

/// Retrieve the verified 64-hex PeerId of a direct peer if Noise handshake is established.
///
/// # Safety
///
/// `short_id` must be a valid null-terminated C string.
/// Returned string must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_get_peer_id(short_id: *const c_char) -> *mut c_char {
    ffi_guard_ptr(|| {
        let s = match c_str_to_string_bounded(short_id, MAX_FFI_STRING_LEN) {
            Ok(s) => s,
            Err(e) => return record_and_null(e),
        };
        let rt = match require_rt() {
            Ok(r) => r,
            Err(e) => return record_and_null(e),
        };
        match rt.block_on(api::get_peer_id(s)) {
            Ok(pid) => string_to_c_char(pid),
            Err(e) => record_and_null(e),
        }
    })
}

/// Free a C string allocated by Bitchat core.
///
/// # Safety
///
/// `ptr` must be a valid pointer allocated by `CString::into_raw` or null.
#[no_mangle]
pub unsafe extern "C" fn bitchat_free_string(ptr: *mut c_char) {
    ffi_guard_void(|| {
        if !ptr.is_null() {
            drop(CString::from_raw(ptr));
        }
    });
}

/// Free raw byte buffer allocated by Bitchat core via `Box<[u8]>`.
///
/// # Safety
///
/// `ptr` and `len` must correspond to a buffer allocated by [`bitchat_poll_outbound`], or null.
#[no_mangle]
pub unsafe extern "C" fn bitchat_free_bytes(ptr: *mut u8, len: usize) {
    ffi_guard_void(|| {
        if !ptr.is_null() && len > 0 {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
        }
    });
}
