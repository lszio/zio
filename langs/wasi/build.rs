// The payload is compiled into the module per application: WASI gives a
// module no way to read its own file, so a single-file .wasm has to
// carry the bundle in memory. `zio build --target wasm-wasi` points
// ZIO_WASI_PAYLOAD at the payload record; a plain build has none.
fn main() {
    println!("cargo:rerun-if-env-changed=ZIO_WASI_PAYLOAD");
    let payload = std::env::var("ZIO_WASI_PAYLOAD").unwrap_or_default();
    let generated = match payload.is_empty() {
        true => "pub const PAYLOAD: &str = \"\";".to_string(),
        false => format!(
            "pub const PAYLOAD: &str = include_str!(r\"{}\");",
            std::fs::canonicalize(&payload)
                .expect("payload file exists")
                .display()
        ),
    };
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(out.join("payload.rs"), generated).expect("write generated payload");
}
