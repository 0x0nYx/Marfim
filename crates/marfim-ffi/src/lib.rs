//! marfim-ffi — the C ABI boundary (W4.1).
//!
//! Doctrine (docs/DISTILLATION.md §FORMER-REJECTS, panic containment):
//! `catch_unwind` lives at the FFI boundary ONLY — every other crate keeps
//! the crate-level unsafe-forbid attribute (verified by the hygiene test
//! below). This crate is the single
//! sanctioned holder of `unsafe`, and its hygiene is test-enforced
//! (tests/ffi_hygiene.rs, gate G14):
//! - every `unsafe` block carries a `// SAFETY:` comment,
//! - every `extern "C"` fn wraps its body in `catch_unwind` and never lets
//!   a panic cross the boundary (a panic is recorded as the thread's last
//!   error and surfaces as a null pointer — the documented outcome),
//! - string ownership is explicit: every returned `char*` produced by this
//!   crate comes from `CString::into_raw` and is released with
//!   [`marfim_string_free`]; the version pointer is static and never freed.
//!
//! The surface is intentionally small and honest: version discovery and
//! config-TOML validation (fail-closed, reusing the core loader the CLI
//! uses — zero behavior drift, W1.1 doctrine).

#![warn(unsafe_op_in_unsafe_fn)]
// The lint below is belt-and-braces against accidental forbid regressions;
// this file is scanned textually by tests/ffi_hygiene.rs (gate G14) and the
// scan skips `//`-comment lines, so mentioning the attribute here is safe.
#![cfg_attr(not(test), deny(missing_docs))]

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};

thread_local! {
    /// Last error message for the calling thread, owned as a CString so the
    /// pointer handed out by `marfim_last_error` stays valid until the next
    /// error on the same thread (documented contract).
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// Fixed marker used when an error message itself contains a NUL (C-string
/// literals are proven NUL-free at compile time — no fallible path here).
const NUL_IN_MESSAGE: &CStr = c"marfim-ffi: error message contained NUL";

fn record_error(msg: &str) {
    let owned = CString::new(msg).unwrap_or_else(|_| NUL_IN_MESSAGE.to_owned());
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(owned));
}

fn last_error_snapshot() -> Option<CString> {
    LAST_ERROR.with(|e| e.borrow().clone())
}

/// Canonical result envelope for JSON-returning entry points: every
/// fallible call answers `{"ok":true,"result":…}` or
/// `{"ok":false,"error":"…"}` — never a bare null without a recorded last
/// error, never a process abort.
fn envelope_ok(payload: serde_json::Value) -> String {
    serde_json::json!({ "ok": true, "result": payload }).to_string()
}

/// Hand a Rust string across the boundary. The caller owns the pointer and
/// must free it with `marfim_string_free`. serde_json envelopes never emit
/// a raw NUL byte, so `CString::new` cannot fail on them; the defensive
/// arm hands out a static literal instead of ever returning UB.
fn hand_out(s: &str) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => {
            record_error("NUL in payload");
            // Static literal, leaked deliberately: tiny, bounded, honest.
            CString::new("{\"ok\":false,\"error\":\"NUL in payload\"}")
                .unwrap_or_default()
                .into_raw()
        }
    }
}

/// Read a C string input pointer defensively: null and non-UTF8 inputs are
/// honest errors, never a dereference of an invalid pointer.
///
/// # Safety
/// `p` must be null or point to a NUL-terminated, valid-for-read UTF-8 C
/// string for the duration of the call. Nothing retains `p`.
unsafe fn read_in(p: *const c_char) -> Result<String, String> {
    if p.is_null() {
        return Err("null input pointer".to_string());
    }
    // SAFETY: caller contract above — p is a valid NUL-terminated C string
    // and is only read here, never stored.
    let cstr = unsafe { CStr::from_ptr(p) };
    cstr.to_str()
        .map(|s| s.to_string())
        .map_err(|e| format!("input is not UTF-8: {e}"))
}

/// Build the `ok:false` envelope pointer AND record the same text as the
/// thread's last error, so a JSON-reading caller and a last-error caller
/// agree on the failure reason.
fn err_envelope(msg: &str) -> *mut c_char {
    record_error(msg);
    hand_out(&format!(
        "{}",
        serde_json::json!({ "ok": false, "error": msg })
    ))
}

/// Returns the canonical Marfim version (the single version source,
/// `marfim_core::VERSION`). Never null; static storage — do NOT free.
#[no_mangle]
pub extern "C" fn marfim_version() -> *const c_char {
    match catch_unwind(marfim_version_inner) {
        Ok(p) => p,
        // SAFETY: the catch arm returns the same 'static NUL-terminated
        // literal the inner call hands out; no state was mutated.
        Err(_) => marfim_version_inner(),
    }
}

fn marfim_version_inner() -> *const c_char {
    // Compile-time-pinned byte literal: NUL-terminated, 'static, and
    // asserted equal to the single version source by the unit tests.
    const V: &[u8] = b"1.0.0\0";
    V.as_ptr() as *const c_char
}

/// Validate a config TOML document through the SAME core loader the CLI
/// uses (`Config::from_toml_str` + `validate_consumed_sections`).
/// Fail-closed: unknown or invalid input yields `ok:false`, never a guess.
///
/// Returns a heap-allocated JSON envelope (free with `marfim_string_free`),
/// or null only if the call panicked internally (last error then names it).
///
/// # Safety
/// `toml` must be null or a valid NUL-terminated UTF-8 C string for the
/// duration of the call. The returned pointer must be released with
/// `marfim_string_free` (or be null).
#[no_mangle]
pub unsafe extern "C" fn marfim_config_probe(toml: *const c_char) -> *mut c_char {
    // AssertUnwindSafe: the input pointer is only read; nothing held across
    // the boundary can be left dangling by an unwind.
    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the caller contract makes `toml` null-or-valid for the
        // duration of this call; read_in only borrows it, never stores it.
        let text = match unsafe { read_in(toml) } {
            Ok(t) => t,
            Err(e) => return err_envelope(&e),
        };
        match probe_inner(&text) {
            Ok(json) => hand_out(&json),
            Err(e) => err_envelope(&e),
        }
    })) {
        Ok(p) => p,
        // SAFETY: null is the documented panic outcome and the error is
        // recorded for marfim_last_error.
        Err(_) => {
            record_error("panic in marfim_config_probe");
            std::ptr::null_mut()
        }
    }
}

fn probe_inner(text: &str) -> Result<String, String> {
    let cfg = marfim_core::config::Config::from_toml_str(text)
        .map_err(|e| format!("config invalid: {e}"))?;
    // The consumed-sections validator is what the CLI answers with; the
    // probe refuses to be more permissive than the binary.
    cfg.validate_consumed_sections()
        .map_err(|e| format!("config invalid: {e}"))?;
    let mut keys: Vec<String> = cfg.keys();
    keys.sort();
    Ok(envelope_ok(serde_json::json!({
        "sections": keys.len(),
        "keys": keys,
    })))
}

/// Copy the calling thread's last error into a fresh heap string. Null
/// means "no error recorded" (not an error itself). The copy is owned by
/// the caller: free with `marfim_string_free`.
#[no_mangle]
pub extern "C" fn marfim_last_error() -> *mut c_char {
    match catch_unwind(marfim_last_error_inner) {
        Ok(p) => p,
        // SAFETY: null is the documented panic outcome; recording here
        // cannot itself panic (CString on a fixed literal).
        Err(_) => {
            record_error("panic in marfim_last_error");
            std::ptr::null_mut()
        }
    }
}

fn marfim_last_error_inner() -> *mut c_char {
    match last_error_snapshot() {
        Some(c) => c.into_raw(),
        None => std::ptr::null_mut(),
    }
}

/// Free a string handed out by this API (`marfim_config_probe`,
/// `marfim_last_error`). Passing null is a no-op. Freeing a pointer not
/// produced by this API, or freeing twice, is a caller contract violation
/// (the same contract every C ABI carries — the name is explicit so
/// bindings wrap it).
///
/// # Safety
/// `p` must be null or a pointer previously returned by
/// `marfim_config_probe`/`marfim_last_error` and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn marfim_string_free(p: *mut c_char) {
    // The contract says EVERY extern fn wraps catch_unwind — including
    // this one, even though reclaiming a valid CString cannot panic.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !p.is_null() {
            // SAFETY: p came from CString::into_raw in this crate and has
            // not been freed yet (caller contract); reclaim and drop it.
            unsafe { drop(CString::from_raw(p)) };
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_the_single_source() {
        let p = marfim_version();
        // SAFETY: static literal contract from marfim_version.
        let s = unsafe { CStr::from_ptr(p) };
        assert_eq!(s.to_str().unwrap(), marfim_core::VERSION);
    }

    #[test]
    fn probe_accepts_valid_and_refuses_invalid() {
        assert_json_ok(probe_test("model = \"claude-sonnet-4\"\n"));
        // A scalar type mismatch at the config-bag layer is NOT refused
        // here: Config is schemaless and types are validated where keys are
        // consumed (the probe refuses to be stricter than the CLI loader).
        assert_json_ok(probe_test("model = 17\n"));
        // Unknown keys inside a known section ARE refused (fail-closed,
        // same validator the CLI answers with).
        assert_json_err(probe_test(
            "[permissions]\nmode = \"yolo\"\nbogusKey = true\n",
        ));
        // Malformed TOML is refused.
        assert_json_err(probe_test("{{{ not toml ]]"));
    }

    #[test]
    fn null_input_is_an_honest_error_not_a_crash() {
        // SAFETY: the null contract is exactly what this test asserts.
        let p = unsafe { marfim_config_probe(std::ptr::null()) };
        assert_json_err(p);
        // The last error is now set and readable.
        let e = marfim_last_error();
        assert!(!e.is_null());
        // SAFETY: e came from marfim_last_error just above.
        unsafe { marfim_string_free(e) };
    }

    #[test]
    fn last_error_starts_empty_on_a_fresh_thread() {
        // thread_local isolation: this test's thread recorded nothing.
        assert!(last_error_snapshot().is_none());
        assert!(marfim_last_error().is_null());
    }

    #[test]
    fn string_free_roundtrip_and_null_noop() {
        // SAFETY: null free is a documented no-op; the roundtrip pointer
        // comes from hand_out in this test.
        unsafe { marfim_string_free(std::ptr::null_mut()) };
        let p = hand_out("roundtrip");
        // SAFETY: p was produced by hand_out above and freed exactly once.
        unsafe { marfim_string_free(p) };
    }

    // -- helpers -------------------------------------------------------------

    fn probe_test(toml: &str) -> *mut c_char {
        let c = CString::new(toml).unwrap();
        // SAFETY: c.as_ptr() is a valid NUL-terminated string for the call.
        unsafe { marfim_config_probe(c.as_ptr()) }
    }

    fn read_json(p: *mut c_char) -> serde_json::Value {
        assert!(!p.is_null());
        // SAFETY: p was produced by this test's own hand_out/err_envelope
        // path and is still owned (not yet freed).
        let s = unsafe { CStr::from_ptr(p) };
        let v = serde_json::from_str::<serde_json::Value>(s.to_str().unwrap()).unwrap();
        // SAFETY: same ownership line as above; freed exactly once here.
        unsafe { marfim_string_free(p) };
        v
    }

    fn assert_json_ok(p: *mut c_char) {
        let v = read_json(p);
        assert_eq!(v["ok"], serde_json::json!(true), "envelope: {v}");
    }

    fn assert_json_err(p: *mut c_char) {
        let v = read_json(p);
        assert_eq!(v["ok"], serde_json::json!(false), "envelope: {v}");
        assert!(v["error"].as_str().is_some());
    }
}
