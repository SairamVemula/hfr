// Embeds the app icon and version info into the Windows executable.
fn main() {
    #[cfg(all(windows, target_env = "msvc"))]
    println!("cargo:rustc-link-arg-bins=/STACK:16777216");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/branding/icon.ico");
        res.set("ProductName", "HFR Remote");
        res.set("FileDescription", "Render Hyperframes projects on your own VPS");
        res.set("LegalCopyright", "Copyright (c) HFR Remote contributors");
        if let Err(e) = res.compile() {
            // A missing resource compiler must not break the build; the exe just keeps the default icon.
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
