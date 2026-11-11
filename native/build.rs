fn main() {
    println!("cargo:rerun-if-changed=src/encoder.c");
    cc::Build::new().file("src/encoder.c").compile("encoder");
    println!("cargo:rustc-link-lib=x264");
}
