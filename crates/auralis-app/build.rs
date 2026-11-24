use slint_build::{CompilerConfiguration, EmbedResourcesKind};

fn main() {
    // Las imágenes de la interfaz (icono de ventana) van dentro del ejecutable:
    // así `acustia-player.exe` funciona al moverlo de carpeta.
    let config = CompilerConfiguration::new().embed_resources(EmbedResourcesKind::EmbedFiles);
    slint_build::compile_with_config("ui/main.slint", config)
        .expect("error al compilar la interfaz Slint");

    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    // Icono del ejecutable (explorador de archivos, barra de tareas, Alt+Tab).
    // Se decide por el sistema de DESTINO, no por el que compila.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Acustia Player");
        res.set("FileDescription", "Acustia Player - reproductor de música local");
        res.set("LegalCopyright", "Acustia Player");
        res.compile()
            .expect("no se pudo incrustar el icono en el ejecutable (¿falta el Windows SDK / rc.exe?)");
    }
}
