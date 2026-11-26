use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    fs::copy("memory.x", out.join("memory.x")).expect("memory.x is readable");
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=src/lfs_glue.c");
    println!("cargo:rerun-if-changed=littlefs");

    // The stock build is CMake Release, so littlefs's asserts and logging are compiled out.
    cc::Build::new()
        .files(["littlefs/lfs.c", "littlefs/lfs_util.c", "src/lfs_glue.c"])
        .include("littlefs")
        .define("LFS_NO_MALLOC", None)
        .define("LFS_NO_ASSERT", None)
        .define("LFS_NO_DEBUG", None)
        .define("LFS_NO_WARN", None)
        .define("LFS_NO_ERROR", None)
        .flag("-std=c99")
        .flag("-ffreestanding")
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        .compile("littlefs");
}
