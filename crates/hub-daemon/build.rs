//! Embeds the ControlAnything icon and name into `controlanythingd.exe` (Windows); no effect elsewhere.

fn main() {
    let icon = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/icons/controlanything.ico"
    );
    println!("cargo:rerun-if-changed={icon}");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(icon);
        // Task Manager shows FileDescription (default: the crate name "hub-daemon").
        resource.set("FileDescription", "ControlAnything Service");
        resource.set("ProductName", "ControlAnything");
        if let Err(err) = resource.compile() {
            println!("cargo:warning=executable resources not embedded: {err}");
        }
    }
    #[cfg(not(windows))]
    let _ = icon;
}
