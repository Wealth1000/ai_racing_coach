// Build script — the only build-time step is embedding the application icon
// into the Windows executable. On every other target it does nothing but
// declare its inputs, so Linux/macOS builds are byte-identical to a repo
// without this file.

fn main() {
    // CARGO_CFG_TARGET_OS is set for the *target*, so a Linux host
    // cross-compiling for Windows still gets the resource — and a native
    // Linux build never invokes the toolchain winresource doesn't need.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/logo.ico");
        resource
            .compile()
            .expect("embed assets/logo.ico into the Windows executable");
    }

    // Without these, any change to the inputs would not rebuild the resource.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/logo.ico");
    println!("cargo:rerun-if-changed=assets/icon_256.png");
}
