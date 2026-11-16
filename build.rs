fn main() {
    println!("cargo:rerun-if-changed=assets/icon/poetrhymer.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("assets/icon/poetrhymer.ico")
            .set("ProductName", "Poetrhymer")
            .set("FileDescription", "Poetrhymer — редактор стихов и песен")
            .set("InternalName", "Poetrhymer")
            .set("OriginalFilename", "Poetrhymer.exe");
        resource
            .compile()
            .expect("Failed to compile Windows icon and version resources");
    }
}
