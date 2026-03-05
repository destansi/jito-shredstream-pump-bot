fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_client(true)
        .build_server(false)
        .compile(&["proto/shredstream.proto"], &["proto"])?;
    println!("cargo:rerun-if-changed=proto/shredstream.proto");
    Ok(())
}
