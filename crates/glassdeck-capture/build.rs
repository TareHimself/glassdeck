use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const CEF_HINT: &str = "CEF not found: extract a windows64 minimal build from https://cef-builds.spotifycdn.com/index.html \
     into third_party/cef, or point GLASSDECK_CEF at one";

fn main() {
    println!("cargo:rerun-if-changed=src/web_shim.cpp");
    println!("cargo:rerun-if-env-changed=GLASSDECK_CEF");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let cef = std::env::var_os("GLASSDECK_CEF")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../third_party/cef"));
    assert!(cef.join("include/cef_app.h").exists(), "{CEF_HINT} (looked in {})", cef.display());

    if std::env::var_os("CMAKE").is_none() {
        let bundled = visual_studio_cmake().expect("set CMAKE to a cmake.exe that knows your Visual Studio version");
        // SAFETY: build scripts are single-threaded at this point.
        unsafe { std::env::set_var("CMAKE", bundled) };
    }
    // Rust links the dynamic C runtime (/MD), so the wrapper must too; CEF defaults to /MT.
    let wrapper = cmake::Config::new(&cef)
        .profile("Release")
        .define("CEF_RUNTIME_LIBRARY_FLAG", "/MD")
        .build_target("libcef_dll_wrapper")
        .build();
    println!("cargo:rustc-link-search=native={}", wrapper.join("build/libcef_dll_wrapper/Release").display());
    println!("cargo:rustc-link-lib=static=libcef_dll_wrapper");

    cc::Build::new()
        .cpp(true)
        .file("src/web_shim.cpp")
        .include(&cef)
        .flag("/std:c++20")
        .define("NOMINMAX", None)
        .define("WIN32_LEAN_AND_MEAN", None)
        .define("UNICODE", None)
        .define("_UNICODE", None)
        .compile("glassdeck_web_shim");

    println!("cargo:rustc-link-search=native={}", cef.join("Release").display());
    println!("cargo:rustc-link-lib=dylib=libcef");
    // Only `web` touches CEF, so `stream` and `play` start without libcef.dll present.
    println!("cargo:rustc-link-lib=dylib=delayimp");
    println!("cargo:rustc-link-arg-bins=/DELAYLOAD:libcef.dll");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let profile_dir = out_dir
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "build"))
        .and_then(Path::parent)
        .expect("OUT_DIR is inside target/<profile>/build")
        .to_path_buf();
    copy_runtime(&cef.join("Release"), &profile_dir);
    copy_runtime(&cef.join("Resources"), &profile_dir);
}

/// A standalone CMake can predate the installed Visual Studio; the copy Visual Studio ships always matches it.
fn visual_studio_cmake() -> Option<PathBuf> {
    let vswhere = PathBuf::from(std::env::var_os("ProgramFiles(x86)")?).join("Microsoft Visual Studio/Installer/vswhere.exe");
    let output = Command::new(vswhere).args(["-latest", "-products", "*", "-property", "installationPath"]).output().ok()?;
    let install = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    let cmake = install.join("Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin/cmake.exe");
    cmake.exists().then_some(cmake)
}

/// CEF loads its DLLs, data files and locales from next to the executable.
fn copy_runtime(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).unwrap_or_else(|e| panic!("{CEF_HINT}: {}: {e}", from.display())) {
        let entry = entry.unwrap();
        let source = entry.path();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_runtime(&source, &target);
        } else if source.extension().is_none_or(|ext| ext != "lib") && !same_size(&source, &target) {
            fs::copy(&source, &target).unwrap_or_else(|e| panic!("copying {}: {e}", source.display()));
        }
    }
}

fn same_size(a: &Path, b: &Path) -> bool {
    matches!((fs::metadata(a), fs::metadata(b)), (Ok(a), Ok(b)) if a.len() == b.len())
}
