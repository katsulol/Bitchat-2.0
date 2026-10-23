use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::OnceLock;
use tokio::runtime::Runtime;

use crate::api;

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

unsafe fn c_str_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    CStr::from_ptr(ptr).to_str().ok().map(|s| s.to_string())
}

fn string_to_c_char(s: String) -> *mut c_char {
    CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Initialize Bitchat core with persistent identity and keystore.
///
/// # Safety
///
/// Caller must provide a valid null-terminated C string for `storage_path`.
/// The returned pointer must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_init(storage_path: *const c_char) -> *mut c_char {
    let path = match c_str_to_string(storage_path) {
        Some(p) => p,
        None => return std::ptr::null_mut(),
    };

    let rt = match get_rt() {
        Some(r) => r,
        None => return std::ptr::null_mut(),
    };

    match rt.block_on(api::init(path)) {
        Ok(short_id) => string_to_c_char(short_id),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Return own 8-byte hexadecimal ShortId.
///
/// # Safety
///
/// Returned pointer must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_my_short_id() -> *mut c_char {
    let rt = match get_rt() {
        Some(r) => r,
        None => return std::ptr::null_mut(),
    };

    match rt.block_on(api::my_short_id()) {
        Ok(id) => string_to_c_char(id),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Return own full 32-byte hexadecimal public key.
///
/// # Safety
///
/// Returned pointer must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_my_peer_id() -> *mut c_char {
    let rt = match get_rt() {
        Some(r) => r,
        None => return std::ptr::null_mut(),
    };

    match rt.block_on(api::my_peer_id()) {
        Ok(pk) => string_to_c_char(pk),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Broadcast a message to all mesh peers.
///
/// # Safety
///
/// `text` must be a valid null-terminated C string. Returned pointer must be freed via [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_send_broadcast(text: *const c_char) -> *mut c_char {
    let body = match c_str_to_string(text) {
        Some(b) => b,
        None => return std::ptr::null_mut(),
    };

    let rt = match get_rt() {
        Some(r) => r,
        None => return std::ptr::null_mut(),
    };

    match rt.block_on(api::send_broadcast(body)) {
        Ok(msg_id) => string_to_c_char(msg_id),
        Err(_) => std::ptr::null_mut(),
    }
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
    let target = match c_str_to_string(recipient) {
        Some(t) => t,
        None => return std::ptr::null_mut(),
    };
    let body = match c_str_to_string(text) {
        Some(b) => b,
        None => return std::ptr::null_mut(),
    };

    let rt = match get_rt() {
        Some(r) => r,
        None => return std::ptr::null_mut(),
    };

    match rt.block_on(api::send_to(target, body)) {
        Ok(msg_id) => string_to_c_char(msg_id),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Notify mesh of peer connection.
///
/// # Safety
///
/// `short_id` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn bitchat_on_peer_connected(short_id: *const c_char) -> i32 {
    let peer = match c_str_to_string(short_id) {
        Some(p) => p,
        None => return -1,
    };

    let rt = match get_rt() {
        Some(r) => r,
        None => return -1,
    };

    match rt.block_on(api::on_peer_connected(peer)) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

/// Notify mesh of peer disconnection.
///
/// # Safety
///
/// `short_id` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn bitchat_on_peer_disconnected(short_id: *const c_char) -> i32 {
    let peer = match c_str_to_string(short_id) {
        Some(p) => p,
        None => return -1,
    };

    let rt = match get_rt() {
        Some(r) => r,
        None => return -1,
    };

    match rt.block_on(api::on_peer_disconnected(peer)) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

/// Feed an incoming raw wire frame into the mesh.
///
/// # Safety
///
/// `short_id` must be null-terminated. `data` must point to a buffer of at least `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn bitchat_on_frame_received(
    short_id: *const c_char,
    data: *const u8,
    len: usize,
) -> i32 {
    let peer = match c_str_to_string(short_id) {
        Some(p) => p,
        None => return -1,
    };

    if data.is_null() || len == 0 {
        return -1;
    }

    let bytes = std::slice::from_raw_parts(data, len).to_vec();
    let rt = match get_rt() {
        Some(r) => r,
        None => return -1,
    };

    match rt.block_on(api::on_frame_received(peer, bytes)) {
        Ok(_) => 0,
        Err(_) => -1,
    }
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
    if out_target.is_null() || out_data.is_null() || out_len.is_null() {
        return -1;
    }

    let rt = match get_rt() {
        Some(r) => r,
        None => return -1,
    };

    match rt.block_on(api::poll_outbound()) {
        Ok(Some(frame)) => {
            if let Some(to) = frame.to {
                *out_target = string_to_c_char(to);
            } else {
                *out_target = std::ptr::null_mut();
            }

            let len = frame.bytes.len();
            *out_len = len;

            let mut boxed = frame.bytes.into_boxed_slice();
            let raw_ptr = boxed.as_mut_ptr();
            std::mem::forget(boxed);
            *out_data = raw_ptr;

            1 // Frame available
        }
        Ok(None) => 0, // No frames
        Err(_) => -1,
    }
}

/// Poll received inbound message for UI presentation.
///
/// # Safety
///
/// `out_json` must be a valid, non-null pointer to a `*mut c_char`.
/// Returned JSON string must be freed using [`bitchat_free_string`].
#[no_mangle]
pub unsafe extern "C" fn bitchat_poll_inbound(out_json: *mut *mut c_char) -> i32 {
    if out_json.is_null() {
        return -1;
    }

    let rt = match get_rt() {
        Some(r) => r,
        None => return -1,
    };

    match rt.block_on(api::poll_inbound()) {
        Ok(Some(msg)) => {
            let recip_json = match msg.recipient {
                Some(r) => format!("\"{}\"", r),
                None => "null".to_string(),
            };
            let escaped_text = msg
                .text
                .replace('\\', "\\\\")
                .replace('\"', "\\\"")
                .replace('\n', "\\n");
            let json = format!(
                "{{\"msg_id\":\"{}\",\"sender\":\"{}\",\"recipient\":{},\"text\":\"{}\",\"hop_count\":{},\"is_broadcast\":{}}}",
                msg.msg_id, msg.sender, recip_json, escaped_text, msg.hop_count, msg.is_broadcast
            );
            *out_json = string_to_c_char(json);
            1 // Message available
        }
        Ok(None) => 0, // No messages
        Err(_) => -1,
    }
}

/// Free a C string allocated by Bitchat core.
///
/// # Safety
///
/// `ptr` must be a valid pointer allocated by `CString::into_raw` or null.
#[no_mangle]
pub unsafe extern "C" fn bitchat_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(CString::from_raw(ptr));
    }
}

/// Free raw byte buffer allocated by Bitchat core.
///
/// # Safety
///
/// `ptr` and `len` must correspond to a buffer allocated via `Box::into_raw` or null.
#[no_mangle]
pub unsafe extern "C" fn bitchat_free_bytes(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len > 0 {
        drop(Vec::from_raw_parts(ptr, len, len));
    }
}
