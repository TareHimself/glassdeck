fn main() {
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rerun-if-changed=src/linux/mpp_shim.c");
        let include_paths = pkg_config::Config::new()
            .cargo_metadata(false)
            .probe("rockchip_mpp")
            .expect("Rockchip MPP not found; build and install https://github.com/rockchip-linux/mpp first")
            .include_paths;
        cc::Build::new()
            .file("src/linux/mpp_shim.c")
            .includes(&include_paths)
            .warnings(true)
            .compile("glassdeck_mpp_shim");
        // The shim must precede MPP on the link line, or --as-needed drops MPP.
        pkg_config::probe_library("rockchip_mpp").expect("Rockchip MPP not found");
    }
}
