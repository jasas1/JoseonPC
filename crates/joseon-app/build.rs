// VERSIONINFO is written only for a Windows build that will be signed.
// An ordinary build, and a cross-check from another system, does not call the
// resource compiler (that compiler is not there unless the Windows SDK is).
fn main() {
    println!("cargo:rerun-if-env-changed=JOSEON_REQUIRE_SIGNATURE");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref() == Some("windows");
    let sign = std::env::var("JOSEON_REQUIRE_SIGNATURE").ok().as_deref() == Some("1");
    if !windows || !sign {
        return;
    }
    // SignPath compares these strings with the artifact configuration. The product
    // name is Joseon, not the crate name. Both version strings are the Cargo version.
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    winresource::WindowsResource::new()
        .set("ProductName", "Joseon")
        .set("FileDescription", "Joseon")
        .set("InternalName", "Joseon.exe")
        .set("OriginalFilename", "Joseon.exe")
        .set("ProductVersion", &version)
        .set("FileVersion", &version)
        .compile()
        .expect("VERSIONINFO is required when JOSEON_REQUIRE_SIGNATURE=1");
}
