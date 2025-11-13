pub mod multicast_service;

use std::{
    ffi::CStr,
    os::raw::c_char,
    ptr,
    sync::Arc,
};
use once_cell::sync::OnceCell;
use tokio::runtime::Runtime;

use crate::multicast_service::LanDiscovery;

// Global Tokio runtime for FFI callers
static RUNTIME: OnceCell<Runtime> = OnceCell::new();
fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("lan-discovery")
            .build()
            .expect("Failed to build Tokio runtime")
    })
}

// Opaque handle exposed to FFI
pub struct DiscoveryHandle {
    discovery: Arc<LanDiscovery>,
}

impl DiscoveryHandle {
    fn new(discovery: Arc<LanDiscovery>) -> Self { Self { discovery } }
}

#[no_mangle]
pub extern "C" fn discovery_new(service_port: u16, name: *const c_char) -> *mut DiscoveryHandle {
    if name.is_null() { return ptr::null_mut(); }
    let cstr = unsafe { CStr::from_ptr(name) };
    let name_str = match cstr.to_str() { Ok(s) => s.to_string(), Err(_) => return ptr::null_mut() };

    // Build discovery inside the global runtime and start it
    let discovery_res = runtime().block_on(LanDiscovery::new(service_port, name_str));
    let discovery = match discovery_res { Ok(d) => Arc::new(d), Err(_) => return ptr::null_mut() };

    // Start background tasks on global runtime
    let d_clone = discovery.clone();
    runtime().spawn(async move { d_clone.start().await; });

    Box::into_raw(Box::new(DiscoveryHandle::new(discovery)))
}

#[no_mangle]
pub extern "C" fn discovery_free(handle: *mut DiscoveryHandle) {
    if handle.is_null() { return; }
    unsafe { drop(Box::from_raw(handle)); }
}

#[no_mangle]
pub extern "C" fn discovery_get_peers_json(handle: *mut DiscoveryHandle, out_ptr: *mut *mut u8, out_len: *mut usize) -> i32 {
    if handle.is_null() || out_ptr.is_null() || out_len.is_null() { return -1; }
    let dh = unsafe { &*handle };

    // Run async peers_json on the runtime and block current thread until done
    let bytes = runtime().block_on(dh.discovery.peers_json());
    let mut v = bytes;
    let len = v.len();
    let ptr_data = v.as_mut_ptr();
    std::mem::forget(v);
    unsafe {
        *out_ptr = ptr_data;
        *out_len = len;
    }
    0
}

#[no_mangle]
pub extern "C" fn discovery_free_buf(ptr_data: *mut u8, len: usize) {
    if ptr_data.is_null() { return; }
    unsafe { let _ = Vec::from_raw_parts(ptr_data, len, len); }
}
