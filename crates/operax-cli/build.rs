use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let i18n_dir = manifest_dir.join("i18n");
    println!("cargo:rerun-if-changed={}", i18n_dir.display());

    let mut catalogs = fs::read_dir(&i18n_dir)
        .expect("read i18n directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .filter(|path| path.file_name().and_then(|name| name.to_str()) != Some("locales.json"))
        .collect::<Vec<_>>();
    catalogs.sort();

    let mut generated = String::from(
        "fn embedded_i18n_catalog(locale: &str) -> Option<&'static str> {\n    match locale {\n",
    );
    for path in catalogs {
        println!("cargo:rerun-if-changed={}", path.display());
        let locale = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("locale file stem");
        generated.push_str(&format!(
            "        {locale:?} => Some(include_str!({path:?})),\n",
            path = path.display().to_string()
        ));
    }
    generated.push_str("        _ => None,\n    }\n}\n");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    fs::write(out_dir.join("embedded_i18n.rs"), generated).expect("write embedded i18n table");
}
