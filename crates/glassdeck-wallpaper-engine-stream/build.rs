fn main() {
    // Only web sources touch CEF, so glassdeckd starts without loading libcef.dll.
    println!("cargo:rustc-link-arg-bins=/DELAYLOAD:libcef.dll");
}
