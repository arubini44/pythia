//! Embeds the icon and version information in pythia.exe (`pythia.rc`, #77).

fn main() {
    println!("cargo:rerun-if-changed=pythia.rc");
    println!("cargo:rerun-if-changed=../../assets/icon/pythia.ico");
    let macros = [
        concat!("PYTHIA_VERSION=\"", env!("CARGO_PKG_VERSION"), "\""),
        concat!("PYTHIA_VERSION_MAJOR=", env!("CARGO_PKG_VERSION_MAJOR")),
        concat!("PYTHIA_VERSION_MINOR=", env!("CARGO_PKG_VERSION_MINOR")),
        concat!("PYTHIA_VERSION_PATCH=", env!("CARGO_PKG_VERSION_PATCH")),
    ];
    // Not built for Windows: nothing to do. On Windows a failing rc.exe fails
    // the build; a cross build without a resource compiler (cargo zigbuild
    // has none) only warns.
    let result = embed_resource::compile("pythia.rc", macros);
    if let embed_resource::CompilationResult::NotAttempted(missing) = &result {
        println!("cargo:warning=pythia.exe is built without its icon: no {missing}");
    }
    result.manifest_optional().unwrap();
}
