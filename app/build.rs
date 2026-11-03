//! Compiles the Blueprint templates to GtkBuilder XML and bundles them,
//! the stylesheet and the icons into a GResource the binary embeds.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data");
    let ui_out = out.join("ui");
    std::fs::create_dir_all(&ui_out).expect("create ui dir");

    let mut ui_files = Vec::new();
    for entry in std::fs::read_dir(data.join("ui")).expect("data/ui") {
        let path = entry.expect("entry").path();
        if path.extension().is_some_and(|ext| ext == "blp") {
            println!("cargo:rerun-if-changed={}", path.display());
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let target = ui_out.join(format!("{name}.ui"));
            let status = Command::new("blueprint-compiler")
                .args(["compile", "--output"])
                .arg(&target)
                .arg(&path)
                .status()
                .expect("blueprint-compiler is needed to build the window (apt install blueprint-compiler)");
            assert!(status.success(), "blueprint-compiler failed on {}", path.display());
            ui_files.push(format!("{name}.ui"));
        }
    }

    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gresources>\n  <gresource prefix=\"/com/rinflow/linux\">\n",
    );
    for file in &ui_files {
        xml.push_str(&format!(
            "    <file compressed=\"true\" preprocess=\"xml-stripblanks\">ui/{file}</file>\n"
        ));
    }
    xml.push_str("    <file compressed=\"true\">style.css</file>\n");
    xml.push_str("  </gresource>\n  <gresource prefix=\"/com/rinflow/linux/icons/scalable/apps\">\n");
    xml.push_str("    <file preprocess=\"xml-stripblanks\" alias=\"com.rinflow.linux.svg\">icons/com.rinflow.linux.svg</file>\n");
    xml.push_str("  </gresource>\n  <gresource prefix=\"/com/rinflow/linux/icons/scalable/actions\">\n");
    xml.push_str("    <file preprocess=\"xml-stripblanks\" alias=\"com.rinflow.linux-symbolic.svg\">icons/com.rinflow.linux-symbolic.svg</file>\n");
    xml.push_str("  </gresource>\n");
    // The Adwaita symbolic icons the window uses (see data/icons/README.md),
    // one GResource prefix per icon context.
    for context in ["actions", "devices", "emblems", "status"] {
        let dir = data.join("icons/scalable").join(context);
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(".svg"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        if names.is_empty() {
            continue;
        }
        xml.push_str(&format!(
            "  <gresource prefix=\"/com/rinflow/linux/icons/scalable/{context}\">\n"
        ));
        for name in names {
            xml.push_str(&format!(
                "    <file preprocess=\"xml-stripblanks\" alias=\"{name}\">icons/scalable/{context}/{name}</file>\n"
            ));
        }
        xml.push_str("  </gresource>\n");
    }
    xml.push_str("</gresources>\n");
    let gresource = out.join("rinflow.gresource.xml");
    std::fs::write(&gresource, xml).expect("write gresource xml");

    println!("cargo:rerun-if-changed={}", data.join("style.css").display());
    println!("cargo:rerun-if-changed={}", data.join("icons").display());
    glib_build_tools::compile_resources(
        &[out.to_str().unwrap(), data.to_str().unwrap()],
        gresource.to_str().unwrap(),
        "rinflow.gresource",
    );
}
