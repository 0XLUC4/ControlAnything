//! Intègre l'icône ControlAnything dans `controlanything.exe` (Windows) ; sans effet ailleurs.

fn main() {
    let icon = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/icons/controlanything.ico"
    );
    println!("cargo:rerun-if-changed={icon}");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        // Id 1 : icône de l'exe, et celle que GPUI charge pour la fenêtre et la barre des tâches.
        resource.set_icon(icon);
        if let Err(err) = resource.compile() {
            // Sans outil de ressources (rc.exe) : exe sans icône plutôt qu'un build en échec.
            println!("cargo:warning=icône de l'exécutable non intégrée : {err}");
        }
    }
    #[cfg(not(windows))]
    let _ = icon;
}
