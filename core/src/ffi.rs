use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::OnceLock;
use tokio::runtime::Runtime;

use crate::api;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn get_rt() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed to build tokio runtime")
    })
}

unsafe fn c_str_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    CStr::from_ptr(ptr).to_str().ok().map(|s| s.to_string())
}

fn string_to_c_char(s: String) -> *mut c_char {
    CString::new(s).map(|c| c.into_raw()).unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn bitchat_init(storage_path: *const c_char) -> *mut c_char {
    let path = match c_str_to_string(storage_path) {
        Some(p) => p,
        None => return std::ptr::null_mut(),
    };

    let rt = get_rt();
    match rt.block_on(api::init(path)) {
        Ok(short_id) => string_to_c_char(short_id),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn bitchat_my_short_id() -> *mut c_char {
    let rt = get_rt();
    match rt.block_on(api::my_short_id()) {
        Ok(id) => string_to_c_char(id),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn bitchat_my_peer_id() -> *mut c_char {
    let rt = get_rt();
    match rt.block_on(api::my_peer_id()) {
        Ok(pk) => string_to_c_char(pk),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn bitchat_send_broadcast(text: *const c_char) -> *mut c_char {
    let body = match c_str_to_string(text) {
        Some(b) => b,
        None => return std::ptr::null_mut(),
    };

    let rt = get_rt();
    match rt.block_on(api::send_broadcast(body)) {
        Ok(msg_id) => string_to_c_char(msg_id),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn bitchat_send_to(recipient: *const c_char, text: *const c_char) -> *mut c_char {
    let target = match c_str_to_string(recipient) {
        Some(t) => t,
        None => return std::ptr::null_mut(),
    };
    let body = match c_str_to_string(text) {
        Some(b) => b,
        None => return std::ptr::null_mut(),
    };

    let rt = get_rt();
    match rt.block_on(api::send_to(target, body)) {
        Ok(msg_id) => string_to_c_char(msg_id),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn bitchat_on_peer_connected(short_id: *const c_char) -> i32 {
    let peer = match c_str_to_string(short_id) {
        Some(p) => p,
        None => return -1,
    };

    let rt = get_rt();
    match rt.block_on(api::on_peer_connected(peer)) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn bitchat_on_peer_disconnected(short_id: *const c_char) -> i32 {
    let peer = match c_str_to_string(short_id) {
        Some(p) => p,
        None => return -1,
    };

    let rt = get_rt();
    match rt.block_on(api::on_peer_disconnected(peer)) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

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
    let rt = get_rt();
    match rt.block_on(api::on_frame_received(peer, bytes)) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn bitchat_poll_outbound(
    out_target: *mut *mut c_char,
    out_data: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    if out_target.is_null() || out_data.is_null() || out_len.is_null() {
        return -1;
    }

    let rt = get_rt();
    match rt.block_on(api::poll_outbound()) {
        Ok(Some(frame)) => {
            if let Some(to) = frame.to {
                *out_target = string_to_c_char(to);
            } else {
                *out_target = std::ptr::null_mut();
            }

            let mut boxed_slice = frame.bytes.into_boxed_slice();
            *out_len = boxed_slice.len();
            *out_data = boxed_slice.as_mut_ptr();
            std::mem::forget(boxed_slice);
            1 // Frame available
        }
        Ok(None) => 0, // No frames
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn bitchat_poll_inbound(out_json: *mut *mut c_char) -> i32 {
    if out_json.is_null() {
        return -1;
    }

    let rt = get_rt();
    match rt.block_on(api::poll_inbound()) {
        Ok(Some(msg)) => {
            // Encode as simple JSON: {"msg_id":"...","sender":"...","recipient":null/string,"text":"...","hop_count":0,"is_broadcast":true}
            let recip_json = match msg.recipient {
                Some(r) => format!("\"{}\"", r),
                None => "null".to_string(),
            };
            // Escape text for simple json
            let escaped_text = msg.text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
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

#[no_mangle]
pub unsafe extern "C" fn bitchat_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(CString::from_raw(ptr));
    }
}

#[no_mangle]
pub unsafe extern "C" fn bitchat_free_bytes(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len > 0 {
        drop(Vec::from_raw_parts(ptr, len, len));
    }
}
