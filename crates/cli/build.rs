//! Embeds the icon and version information, so that Windows, and anything
//! that inspects a signed file, can tell what this executable is and which
//! release it belongs to. The version comes from the package version.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("../../assets/icon.ico")
        // The same product name as zest.exe: both belong to one product.
        .set("ProductName", "Zest")
        .set("FileDescription", "Zest command line")
        .set("CompanyName", "Zest contributors")
        .set(
            "LegalCopyright",
            "Copyright 2026 Pseudo Nymm and the Zest contributors. Apache License 2.0.",
        );
    if let Err(e) = resource.compile() {
        // A missing resource compiler must not make the build fail; the tool
        // simply ships without embedded details in that case.
        println!("cargo:warning=could not embed Windows resources: {e}");
    }
}
