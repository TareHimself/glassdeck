use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// CEF comes from silo: the recipe in cef/ builds CEF's C++ wrapper, and the store holds the result.
fn main() {
    println!("cargo:rerun-if-changed=src/web_shim.cpp");
    println!("cargo:rerun-if-env-changed=SILO");
    println!("cargo:rerun-if-changed=cef/silopkg.py");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let silo = env_path("SILO").unwrap_or_else(|| manifest.join("../../../silo"));
    let recipe = manifest.join("cef");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let cef = Cef::from_store(&silo_build(&silo, &recipe, &out_dir.join("silo")));

    cc::Build::new()
        .cpp(true)
        .file("src/web_shim.cpp")
        .include(&cef.headers)
        .flag("/std:c++20")
        .define("NOMINMAX", None)
        .define("WIN32_LEAN_AND_MEAN", None)
        .define("UNICODE", None)
        .define("_UNICODE", None)
        .compile("glassdeck_web_shim");

    for lib in [&cef.wrapper, &cef.import_lib] {
        println!("cargo:rustc-link-search=native={}", lib.parent().unwrap().display());
        println!("cargo:rustc-link-lib={}", lib.file_stem().unwrap().to_string_lossy());
    }
    // Binaries that link this add /DELAYLOAD:libcef.dll, so only web sources need libcef.dll.
    println!("cargo:rustc-link-lib=dylib=delayimp");

    let profile_dir = out_dir
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "build"))
        .and_then(Path::parent)
        .expect("OUT_DIR is inside target/<profile>/build")
        .to_path_buf();
    for dll in &cef.runtime_libs {
        copy_if_changed(dll, &profile_dir.join(dll.file_name().unwrap()));
    }
    for dir in &cef.content {
        copy_tree(dir, &profile_dir);
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).map(PathBuf::from)
}

/// Builds the recipe (a store hit after the first time) and returns the package's store path.
fn silo_build(silo: &Path, recipe: &Path, report: &Path) -> PathBuf {
    let status = Command::new("uv")
        .current_dir(silo)
        .args(["run", "--package", "silo", "silo", "build"])
        .arg(recipe)
        .arg("--report")
        .arg(report)
        .status()
        .unwrap_or_else(|e| panic!("running silo through uv (is uv installed?): {e}"));
    let parsed = read_json(&report.join("report.json"));
    assert!(status.success() && parsed["ok"] == true, "silo failed to build CEF; see {}", report.display());
    PathBuf::from(parsed["store_path"].as_str().expect("report names a store path"))
}

/// What the stored package's manifest says it holds.
struct Cef {
    headers: PathBuf,
    wrapper: PathBuf,
    import_lib: PathBuf,
    runtime_libs: Vec<PathBuf>,
    content: Vec<PathBuf>,
}

impl Cef {
    fn from_store(store: &Path) -> Self {
        let manifest = read_json(&store.join("silo/package.json"));
        let artifacts = manifest["components"]["cef"]["artifacts"].as_array().expect("cef component in the manifest");
        let of_type = |kind: &str| -> Vec<&Value> { artifacts.iter().filter(|a| a["type"] == kind).collect() };
        let at = |artifact: &Value, key: &str| store.join(artifact[key].as_str().expect("artifact path"));
        let shared = of_type("shared_lib");
        let libcef = shared.iter().find(|a| a["name"] == "libcef").expect("libcef in the manifest");
        Self {
            headers: at(of_type("headers")[0], "path"),
            wrapper: at(of_type("static_lib")[0], "path"),
            import_lib: at(libcef, "import"),
            runtime_libs: shared.iter().map(|a| at(a, "path")).collect(),
            content: of_type("content").into_iter().map(|a| at(a, "path")).collect(),
        }
    }
}

fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

/// CEF loads its DLLs, data files and locales from next to the executable.
fn copy_tree(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).unwrap_or_else(|e| panic!("{}: {e}", from.display())) {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_tree(&entry.path(), &target);
        } else {
            copy_if_changed(&entry.path(), &target);
        }
    }
}

fn copy_if_changed(source: &Path, target: &Path) {
    let same = matches!((fs::metadata(source), fs::metadata(target)), (Ok(a), Ok(b)) if a.len() == b.len());
    if !same {
        fs::copy(source, target).unwrap_or_else(|e| panic!("copying {}: {e}", source.display()));
    }
}
