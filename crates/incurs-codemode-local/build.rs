use sha2::{Digest, Sha256};

fn source_fingerprint(root: &std::path::Path) -> Sha256 {
    let provenance: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("provenance.json")).expect("engine provenance"),
    )
    .expect("valid provenance");
    let mut fingerprint = Sha256::new();
    for (name, expected) in provenance["sha256"].as_object().expect("source hashes") {
        let bytes = std::fs::read(root.join(name)).expect("vendored source file");
        let actual = format!("{:x}", Sha256::digest(&bytes));
        assert_eq!(
            expected.as_str(),
            Some(actual.as_str()),
            "Vendored engine source changed: {name}"
        );
        fingerprint.update(name);
        fingerprint.update(bytes);
    }
    fingerprint.update(std::fs::read("native/bridge.c").expect("engine bridge"));
    fingerprint
}

fn main() {
    if std::env::var_os("CARGO_FEATURE_QUICKJS").is_none() {
        return;
    }
    let root = std::path::Path::new("native/quickjs");
    let version = std::fs::read_to_string(root.join("VERSION")).expect("vendored engine version");
    let version = format!("\"{}\"", version.trim());
    println!("cargo:rerun-if-changed=native");
    let mut fingerprint = source_fingerprint(root);
    let mut build = cc::Build::new();
    build
        .include(root)
        .file("native/bridge.c")
        .define("_GNU_SOURCE", None)
        .define("CONFIG_VERSION", version.as_str())
        .flag_if_supported("-std=gnu11")
        .flag_if_supported("-fwrapv")
        .flag_if_supported("-funsigned-char")
        .warnings(false);
    for name in [
        "quickjs.c",
        "libregexp.c",
        "libunicode.c",
        "cutils.c",
        "dtoa.c",
    ] {
        build.file(root.join(name));
    }
    fingerprint.update(format!("{:?}", build.get_compiler().to_command()));
    println!(
        "cargo:rustc-env=INCURS_ENGINE_BUILD={:x}",
        fingerprint.finalize()
    );
    build.compile("incurs_dquickts");
}
