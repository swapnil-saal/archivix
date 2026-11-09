//! Build script: works around a missing link directive in `unrar_sys`.
//!
//! On Windows the bundled UnRAR code calls registry, token and CryptoAPI
//! functions from advapi32 (RegOpenKeyExW, OpenProcessToken, CryptGenRandom),
//! but `unrar_sys` does not ask the linker for advapi32, so MSVC links fail
//! with LNK2019. Linking it here fixes every binary that uses the library.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let rar = std::env::var_os("CARGO_FEATURE_RAR").is_some();
    if windows && rar {
        println!("cargo:rustc-link-lib=advapi32");
    }
}
