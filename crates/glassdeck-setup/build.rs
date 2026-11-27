fn main() {
    let lfs = "../../firmware/littlefs";
    println!("cargo:rerun-if-changed={lfs}");
    println!("cargo:rerun-if-changed=src/lfs_image.c");
    cc::Build::new()
        .files([format!("{lfs}/lfs.c"), format!("{lfs}/lfs_util.c"), "src/lfs_image.c".into()])
        .include(lfs)
        .define("LFS_NO_MALLOC", None)
        .define("LFS_NO_ASSERT", None)
        .define("LFS_NO_DEBUG", None)
        .define("LFS_NO_WARN", None)
        .define("LFS_NO_ERROR", None)
        .warnings(false)
        .compile("lfs_image");
}
