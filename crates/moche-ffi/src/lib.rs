//! `extern "C"` API over moche-core. The only crate allowed to use `unsafe`.
//!
//! No cbindgen yet: its MPL-2.0 licence is pending a decision. Until then the
//! header is hand-written or absent.

/// Returns the library version as a NUL-terminated static string.
#[no_mangle]
pub extern "C" fn moche_version() -> *const std::ffi::c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_nul_terminated() {
        let s = unsafe { std::ffi::CStr::from_ptr(super::moche_version()) };
        assert_eq!(s.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
    }
}
