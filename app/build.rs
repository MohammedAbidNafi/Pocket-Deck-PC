fn main() {
    // The Slint compiler recurses deeply on nested UI; Windows' default 1 MB main-thread stack
    // is too small for it, so compile on a thread with room to spare.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
            slint_build::compile_with_config("ui/app.slint", config).expect("Slint UI compiles");
        })
        .expect("compiler thread starts")
        .join()
        .expect("Slint UI compiles");

    println!("cargo:rerun-if-changed=assets/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        // Icon resource 1: Explorer, the taskbar and the tray icon all use it.
        res.set_icon("assets/app.ico");
        res.set("ProductName", "PocketDeck");
        res.set("FileDescription", "PocketDeck for PC");
        res.set("CompanyName", "PocketDeck");
        res.set("LegalCopyright", "GNU GPL v3");
        res.compile().expect("Windows resources compile");
    }
}
