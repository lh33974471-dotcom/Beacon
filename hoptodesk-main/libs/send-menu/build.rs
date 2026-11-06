fn main() {
    println!("cargo:rustc-link-arg=/ENTRY:DllMain");
    println!("cargo:rustc-link-arg=/NODEFAULTLIB");
}
