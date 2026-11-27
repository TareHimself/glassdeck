fn main() {
    // Only web sources touch CEF, so the CLI starts without loading libcef.dll.
    println!("cargo:rustc-link-arg-bins=/DELAYLOAD:libcef.dll");
}
