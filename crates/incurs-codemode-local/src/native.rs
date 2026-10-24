use std::ffi::{CStr, CString, c_char, c_int};
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;

use crate::LocalExecutorOptions;

#[repr(C)]
struct NativeEngine {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn cm_alloc() -> *mut NativeEngine;
    fn cm_create(
        engine: *mut NativeEngine,
        source: *const c_char,
        memory: usize,
        stack: usize,
        timeout: f64,
        budget: u32,
    ) -> c_int;
    fn cm_restore(
        engine: *mut NativeEngine,
        bytes: *const u8,
        length: usize,
        memory: usize,
        stack: usize,
        timeout: f64,
        budget: u32,
    ) -> c_int;
    fn cm_pump(engine: *mut NativeEngine) -> c_int;
    fn cm_state(engine: *mut NativeEngine) -> *const c_char;
    fn cm_respond(engine: *mut NativeEngine, response: *const c_char) -> c_int;
    fn cm_snapshot(engine: *mut NativeEngine) -> *const u8;
    fn cm_snapshot_size(engine: *mut NativeEngine) -> usize;
    fn cm_error(engine: *mut NativeEngine) -> *const c_char;
    fn cm_destroy(engine: *mut NativeEngine);
}

pub(crate) struct Engine {
    pointer: NonNull<NativeEngine>,
    _thread: PhantomData<Rc<()>>,
}

impl Engine {
    pub(crate) fn new(
        options: &LocalExecutorOptions,
        source: &str,
        saved: Option<&[u8]>,
    ) -> Result<Self, String> {
        // This non-Send owner confines the C runtime and frees its pointers once.
        let pointer =
            NonNull::new(unsafe { cm_alloc() }).ok_or("Cannot allocate JavaScript engine")?;
        let engine = Self {
            pointer,
            _thread: PhantomData,
        };
        let timeout = options.timeout.as_secs_f64() * 1000.0;
        let result = if let Some(bytes) = saved {
            unsafe {
                cm_restore(
                    pointer.as_ptr(),
                    bytes.as_ptr(),
                    bytes.len(),
                    options.memory_limit,
                    options.stack_size,
                    timeout,
                    u32::MAX,
                )
            }
        } else {
            let source = CString::new(source).map_err(|e| e.to_string())?;
            unsafe {
                cm_create(
                    pointer.as_ptr(),
                    source.as_ptr(),
                    options.memory_limit,
                    options.stack_size,
                    timeout,
                    u32::MAX,
                )
            }
        };
        engine.check(result)?;
        Ok(engine)
    }

    pub(crate) fn pump(&self) -> Result<(), String> {
        self.check(unsafe { cm_pump(self.pointer.as_ptr()) })
    }

    pub(crate) fn state(&self) -> Result<String, String> {
        let pointer = unsafe { cm_state(self.pointer.as_ptr()) };
        if pointer.is_null() {
            return Err(self.error());
        }
        Ok(unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned())
    }

    pub(crate) fn snapshot(&self) -> Result<Vec<u8>, String> {
        let pointer = unsafe { cm_snapshot(self.pointer.as_ptr()) };
        if pointer.is_null() {
            return Err(self.error());
        }
        let size = unsafe { cm_snapshot_size(self.pointer.as_ptr()) };
        Ok(unsafe { std::slice::from_raw_parts(pointer, size) }.to_vec())
    }

    pub(crate) fn respond(&self, response: &str) -> Result<(), String> {
        let response = CString::new(response).map_err(|e| e.to_string())?;
        self.check(unsafe { cm_respond(self.pointer.as_ptr(), response.as_ptr()) })
    }

    fn check(&self, result: c_int) -> Result<(), String> {
        if result < 0 {
            Err(self.error())
        } else {
            Ok(())
        }
    }

    fn error(&self) -> String {
        let message = unsafe { CStr::from_ptr(cm_error(self.pointer.as_ptr())) }.to_string_lossy();
        if message.contains("CODE_EXECUTION_TIMEOUT") {
            "CODE_EXECUTION_TIMEOUT: maximum uninterrupted JavaScript execution interval exceeded"
                .into()
        } else {
            message.into_owned()
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { cm_destroy(self.pointer.as_ptr()) };
    }
}
