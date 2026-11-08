//! Embeds the ControlAnything icon and name into `controlanything.exe` (Windows); no effect elsewhere.

fn main() {
    let icon = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/icons/controlanything.ico"
    );
    println!("cargo:rerun-if-changed={icon}");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        // Id 1: the exe icon, also the one GPUI loads for the window and the taskbar.
        resource.set_icon(icon);
        // Task Manager shows FileDescription (default: the crate name "hub-app").
        resource.set("FileDescription", "ControlAnything");
        resource.set("ProductName", "ControlAnything");
        if let Err(err) = resource.compile() {
            // Without a resource tool (rc.exe): an exe without icon rather than a failed build.
            println!("cargo:warning=executable icon not embedded: {err}");
        }
    }
    #[cfg(not(windows))]
    let _ = icon;
}
