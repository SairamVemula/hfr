// Embeds the app icon and version info into the Windows executable.
fn main() {
    println!("cargo:rerun-if-changed=../../assets/branding/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/branding/icon.ico");
        res.set("ProductName", "HFR");
        res.set("FileDescription", "HFR preview player");
        res.set("LegalCopyright", "Copyright (c) HFR contributors");
        if let Err(e) = res.compile() {
            // A missing resource compiler must not break the build; the exe just keeps the default icon.
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
