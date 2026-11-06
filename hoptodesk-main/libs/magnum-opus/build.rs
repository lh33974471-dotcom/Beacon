use std::{
    env,
    path::{Path, PathBuf},
};

#[cfg(all(any(target_os = "linux", target_os = "freebsd"), feature = "linux-pkg-config"))]
fn link_pkg_config(name: &str) -> Vec<PathBuf> {
    let lib = pkg_config::probe_library(name)
        .expect(format!(
            "unable to find '{name}' development headers with pkg-config (feature linux-pkg-config is enabled).
            try installing '{name}-dev' from your system package manager.").as_str());

    let mut paths = lib.include_paths.clone();
    for p in &lib.include_paths {
        if let Some(parent) = p.parent() {
            if !paths.contains(&parent.to_path_buf()) {
                paths.push(parent.to_path_buf());
            }
        }
    }
    paths
}

#[cfg(not(all(any(target_os = "linux", target_os = "freebsd"), feature = "linux-pkg-config")))]
fn link_vcpkg(mut path: PathBuf, name: &str) -> PathBuf {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let mut target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    if target_arch == "x86_64" {
        target_arch = "x64".to_owned();
    } else if target_arch == "aarch64" {
        target_arch = "arm64".to_owned();
    }
    let target = if target_os == "macos" && target_arch == "x64" {
        "x64-osx".to_owned()
    } else if target_os == "macos" && target_arch == "arm64" {
        "arm64-osx".to_owned()
    } else if target_os == "windows" {
        format!("{}-windows-static", target_arch)
    } else {
        format!("{}-{}", target_arch, target_os)
    };
    println!("cargo:info={}", target);
    path.push("installed");
    path.push(target);
    println!(
        "{}",
        format!(
            "cargo:rustc-link-lib=static={}",
            name.trim_start_matches("lib")
        )
    );
    println!(
        "{}",
        format!(
            "cargo:rustc-link-search={}",
            path.join("lib").to_str().unwrap()
        )
    );
    let include = path.join("include");
    println!("{}", format!("cargo:include={}", include.to_str().unwrap()));
    include
}

#[cfg(not(all(any(target_os = "linux", target_os = "freebsd"), feature = "linux-pkg-config")))]
fn link_homebrew_m1(name: &str) -> PathBuf {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    if target_os != "macos" || target_arch != "aarch64" {
        panic!("Couldn't find VCPKG_ROOT, also can't fallback to homebrew because it's only for macos aarch64.");
    }
    let mut path = PathBuf::from("/opt/homebrew/Cellar");
    path.push(name);
    let entries = if let Ok(dir) = std::fs::read_dir(&path) {
        dir
    } else {
        panic!("Could not find package in {}. Make sure your homebrew and package {} are all installed.", path.to_str().unwrap(),&name);
    };
    let mut directories = entries
        .into_iter()
        .filter(|x| x.is_ok())
        .map(|x| x.unwrap().path())
        .filter(|x| x.is_dir())
        .collect::<Vec<_>>();
    // Find the newest version.
    directories.sort_unstable();
    if directories.is_empty() {
        panic!(
            "There's no installed version of {} in /opt/homebrew/Cellar",
            name
        );
    }
    path.push(directories.pop().unwrap());
    // Link the library.
    println!(
        "{}",
        format!(
            "cargo:rustc-link-lib=static={}",
            name.trim_start_matches("lib")
        )
    );
    // Add the library path.
    println!(
        "{}",
        format!(
            "cargo:rustc-link-search={}",
            path.join("lib").to_str().unwrap()
        )
    );
    // Add the include path.
    let include = path.join("include");
    println!("{}", format!("cargo:include={}", include.to_str().unwrap()));
    include
}

#[cfg(all(any(target_os = "linux", target_os = "freebsd"), feature = "linux-pkg-config"))]
fn find_package(name: &str) -> Vec<PathBuf> {
    return link_pkg_config(name);
}

#[cfg(not(all(any(target_os = "linux", target_os = "freebsd"), feature = "linux-pkg-config")))]
fn find_package(name: &str) -> Vec<PathBuf> {
    if let Ok(vcpkg_root) = std::env::var("VCPKG_ROOT") {
        vec![link_vcpkg(vcpkg_root.into(), name)]
    } else {
        // Try using homebrew
        vec![link_homebrew_m1(name)]
    }
}

fn generate_bindings(ffi_header: &Path, include_paths: &[PathBuf], ffi_rs: &Path, exact_file: &Path) {
    #[derive(Debug)]
    struct ParseCallbacks;
    impl bindgen::callbacks::ParseCallbacks for ParseCallbacks {
        fn int_macro(&self, name: &str, _value: i64) -> Option<bindgen::callbacks::IntKind> {
            if name.starts_with("OPUS") {
                Some(bindgen::callbacks::IntKind::Int)
            } else {
                None
            }
        }
    }
    let mut b = bindgen::Builder::default()
        .header(ffi_header.to_str().unwrap())
        .parse_callbacks(Box::new(ParseCallbacks))
        .generate_comments(false);

    for dir in include_paths {
        b = b.clang_arg(format!("-I{}", dir.display()));
    }

    match b.generate() {
        Ok(bindings) => {
            bindings.write_to_file(ffi_rs).unwrap();
            let content = std::fs::read_to_string(ffi_rs).unwrap_or_default();
            if content.contains("pub _address") && exact_file.exists() {
                println!("cargo:warning=bindgen produced opaque types, using pre-generated bindings");
                std::fs::copy(exact_file, ffi_rs).unwrap();
            }
        }
        Err(_) => {
            if exact_file.exists() {
                println!("cargo:warning=bindgen failed, using pre-generated bindings");
                std::fs::copy(exact_file, ffi_rs).unwrap();
            } else {
                panic!("bindgen failed and no pre-generated bindings available");
            }
        }
    }
}

fn gen_opus() {
    let includes = find_package("opus");
    let src_dir = env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let src_dir = Path::new(&src_dir);
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let out_dir = Path::new(&out_dir);

    let ffi_header = src_dir.join("opus_ffi.h");
    println!("rerun-if-changed={}", ffi_header.display());
    for dir in &includes {
        println!("rerun-if-changed={}", dir.display());
    }

    let ffi_rs = out_dir.join("opus_ffi.rs");
    let exact_file = src_dir.join("generated").join("opus_ffi.rs");
    generate_bindings(&ffi_header, &includes, &ffi_rs, &exact_file);
}

fn main() {
    gen_opus()
}
