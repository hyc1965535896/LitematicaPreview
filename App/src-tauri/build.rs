#[path = "src/formats.rs"]
mod formats;

fn main() {
    println!("cargo:rerun-if-changed=src/formats.rs");
    let mut include =
        String::from("; Generated from src/formats.rs.\n!macro LP_FOREACH_EXTENSION APPLY\n");
    for extension in formats::EXTENSIONS {
        include.push_str(&format!(
            "  !insertmacro ${{APPLY}} \"{}\" \"{}\"\n",
            &extension[1..],
            extension
        ));
    }
    include.push_str("!macroend\n");
    std::fs::create_dir_all("gen").expect("Unable to create installer metadata directory");
    std::fs::write("gen/installer-extensions.nsh", include)
        .expect("Unable to generate installer extension choices");
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .windows_attributes(
                tauri_build::WindowsAttributes::new().window_icon_path("../../Assets/app.ico"),
            )
            .app_manifest(tauri_build::AppManifest::new().commands(&[
                "bootstrap",
                "choose_file",
                "load_preview",
                "start_preview",
                "next_preview",
                "read_preview",
                "preview_memory",
                "release_preview",
                "cancel_load",
                "register_associations",
                "unregister_associations",
                "show_licenses",
                "export_schematic",
                "block_catalog",
                "block_icons",
            ])),
    )
    .expect("Unable to build Litematica Preview resources");
}
