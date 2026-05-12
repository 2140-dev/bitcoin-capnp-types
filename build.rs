fn main() {
    println!("cargo:rerun-if-changed=capnp");
    capnpc::CompilerCommand::new()
        .src_prefix("capnp")
        .file("capnp/chain.capnp")
        .file("capnp/common.capnp")
        .file("capnp/echo.capnp")
        .file("capnp/handler.capnp")
        .file("capnp/init.capnp")
        .file("capnp/mining.capnp")
        .file("capnp/proxy.capnp")
        .run()
        .unwrap();
}
