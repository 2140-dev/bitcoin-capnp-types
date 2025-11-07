fn main() {
    println!("cargo:rerun-if-changed=capnp");

    let mut cmd = capnpc::CompilerCommand::new();
    cmd.src_prefix("capnp")
        .file("capnp/common.capnp")
        .file("capnp/echo.capnp")
        .file("capnp/init.capnp")
        .file("capnp/mining.capnp")
        .file("capnp/proxy.capnp");

    // Add common import paths for standard schema files (like /capnp/c++.capnp)
    // These paths are checked in order, and the first existing path is used
    let import_paths = [
        "/usr/include",                 // Standard Ubuntu/Debian location
        "/usr/local/include",           // Common location when built from source
        "/opt/homebrew/include",        // macOS Homebrew on Apple Silicon
        "/usr/local/opt/capnp/include", // macOS Homebrew on Intel
    ];

    for path in &import_paths {
        if std::path::Path::new(path).exists() {
            cmd.import_path(path);
        }
    }

    cmd.run().unwrap();
}
