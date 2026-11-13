//! Сборочный шаг core: статические ассеты из `assets/` сжимаются в gzip и
//! складываются в OUT_DIR — page.rs включает их константами (include_bytes!).
//! На прошивку попадает только gz; flate2 живёт в build-/dev-зависимостях и
//! в Xtensa-образ не попадает.
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::{Compression, GzBuilder};

const ASSETS: [&str; 3] = ["index.html", "style.css", "app.js"];

fn main() {
    println!("cargo:rerun-if-changed=assets");
    println!("cargo:rerun-if-changed=build.rs");
    // unix-время сборки — fallback для остановленной DS1307 (core::time::BUILD_EPOCH)
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    println!("cargo:rustc-env=BUILD_EPOCH={epoch}");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    for name in ASSETS {
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(name);
        let bytes = std::fs::read(&src).unwrap_or_else(|e| panic!("asset {:?}: {e}", src));
        let file = File::create(out.join(format!("{name}.gz"))).expect("OUT_DIR write");
        let mut enc = GzBuilder::new().write(file, Compression::best());
        enc.write_all(&bytes).expect("gz write");
        enc.finish().expect("gz finish");
    }
}
