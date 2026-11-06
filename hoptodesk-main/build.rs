#[cfg(feature = "packui")]
use std::path::PathBuf;

#[cfg(feature = "packui")]
fn pack_ui_archive(root: &std::path::Path, out: &std::path::Path) {
    const SUFFIXES: [&str; 5] = [".html", ".css", ".tis", ".jpg", ".png"];
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut children: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read_dir {}: {}", dir.display(), e))
            .filter_map(|e| e.ok())
            .collect();
        children.sort_by_key(|e| e.path());
        for entry in children {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(root) {
                let name = rel.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
                let lower = name.to_ascii_lowercase();
                if SUFFIXES.iter().any(|suf| lower.ends_with(suf)) {
                    let mut data = std::fs::read(&path)
                        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
                    if name == "index.tis" && std::env::var("CARGO_FEATURE_FIPS").is_ok() {
                        data = fips_about(data);
                    }
                    entries.push((name, data));
                }
            }
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"WUIAR1");
    buf.extend_from_slice(&[0u8, 0u8]);
    buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (name, data) in &entries {
        buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
        buf.extend_from_slice(name.as_bytes());
        buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        buf.extend_from_slice(data);
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(out, &buf).unwrap_or_else(|e| panic!("write {}: {}", out.display(), e));
    println!("cargo:warning=packed {} UI files into {}", entries.len(), out.display());
}

#[cfg(feature = "packui")]
fn fips_about(data: Vec<u8>) -> Vec<u8> {
    let text = String::from_utf8(data).expect("index.tis is not UTF-8");
    let needle = r#"Version: " + versionLabel() + "</div>"#;
    assert!(text.contains(needle), "the About version line is missing from index.tis");
    text.replacen(needle, &format!("{}<div .about-version>FIPS mode: on</div>", needle), 1).into_bytes()
}

fn fips_module() {
    if std::env::var("CARGO_FEATURE_FIPS").is_err() {
        return;
    }
    match std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default().as_str() {
        "macos" => println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks"),
        "windows" => {
            const NAME: &str = "aws_lc_fips_0_13_11_crypto.dll";
            let include = std::env::var("DEP_AWS_LC_FIPS_0_13_11_INCLUDE").expect("aws-lc-fips-sys gave no include path");
            let build = std::path::Path::new(&include).parent().unwrap().join("build");
            let mut stack = vec![build.clone()];
            let mut found = None;
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if path.file_name().map_or(false, |n| n == NAME) {
                        found = Some(path);
                    }
                }
            }
            let dll = found.unwrap_or_else(|| panic!("{} not found under {}", NAME, build.display()));
            println!("cargo:rustc-env=HOP_FIPS_MODULE={}", dll.display());
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{}", NAME);
            println!("cargo:rustc-link-arg-bins=delayimp.lib");
        }
        _ => {}
    }
}

#[cfg(windows)]
fn build_windows() {
	let file = "src/platform/windows.cc";
	cc::Build::new().file(file).compile("windows");
	println!("cargo:rustc-link-lib=WtsApi32");
	println!("cargo:rerun-if-changed={}", file);

	let target = std::env::var("TARGET").unwrap_or_default();
	if target.ends_with("-pc-windows-msvc") {
		println!("cargo:rustc-link-arg-bins=/DELAYLOAD:api-ms-win-core-winrt-l1-1-0.dll");
		println!("cargo:rustc-link-arg-bins=/DELAYLOAD:d3dcompiler_47.dll");
		println!("cargo:rustc-link-arg-bins=/DELAYLOAD:cfgmgr32.dll");
		println!("cargo:rustc-link-arg-bins=/DELAYLOAD:mfplat.dll");
		println!("cargo:rustc-link-arg-bins=delayimp.lib");
	}
}

#[cfg(target_os = "ios")]
println!("cargo:rustc-link-lib=framework=SystemConfiguration");

#[cfg(target_os = "macos")]
fn build_mac() {
    let file = "src/platform/macos.mm";
    let mut b = cc::Build::new();
    if let Ok(os_version::OsVersion::MacOS(v)) = os_version::detect() {
        let v = v.version;
        if v.contains("10.14") {
            b.flag("-DNO_InputMonitoringAuthStatus=1");
        }
    }
    b.file(file).compile("macos");
    println!("cargo:rerun-if-changed={}", file);
}

#[cfg(all(windows, feature = "packui"))]
fn build_manifest() {
    use std::io::Write;
//    if std::env::var("PROFILE").unwrap() == "release" {
        let mut res = winres::WindowsResource::new();
        res.set_icon("res/icon.ico")
            .set_language(winapi::um::winnt::MAKELANGID(
                winapi::um::winnt::LANG_ENGLISH,
                winapi::um::winnt::SUBLANG_ENGLISH_US,
            ))
            .set_manifest_file("res/manifest.xml");
        match res.compile() {
            Err(e) => {
                write!(std::io::stderr(), "{}", e).unwrap();
                std::process::exit(1);
            }
            Ok(_) => {}
        }
//    }
}

fn install_android_deps() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target_os != "android" {
        return;
    }
    let mut target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    if target_arch == "x86_64" {
        target_arch = "x64".to_owned();
    } else if target_arch == "x86" {
        target_arch = "x86".to_owned();
    } else if target_arch == "aarch64" {
        target_arch = "arm64".to_owned();
    } else {
        target_arch = "arm".to_owned();
    }
    let target = format!("{}-android", target_arch);
    let vcpkg_root = std::env::var("VCPKG_ROOT").unwrap();
    let mut path: std::path::PathBuf = vcpkg_root.into();
    if let Ok(vcpkg_root) = std::env::var("VCPKG_INSTALLED_ROOT") {
        path = vcpkg_root.into();
    } else {
        path.push("installed");
    }
    path.push(target);
    println!(
        "{}",
        format!(
            "cargo:rustc-link-search={}",
            path.join("lib").to_str().unwrap()
        )
    );
    println!("cargo:rustc-link-lib=c++");
    println!("cargo:rustc-link-lib=OpenSLES");
}


fn send_menu_class_id() {
    use md5::Digest;
    let toml = std::fs::read_to_string("Cargo.toml").unwrap_or_default();
    let product = toml
        .lines()
        .find_map(|line| line.strip_prefix("ProductName = \""))
        .map(|rest| rest.trim_end_matches('"'))
        .unwrap_or("HopToDesk");
    let hex = format!("{:X}", md5::Md5::digest(product.as_bytes()));
    println!(
        "cargo:rustc-env=SEND_MENU_CLSID={{{}-{}-{}-{}-{}}}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    );
}

fn main() {
    println!("cargo:rerun-if-env-changed=BEACON_FLEET_PASSWORD");
    send_menu_class_id();
    hbb_common::gen_version();
    install_android_deps();
    fips_module();

    #[cfg(all(feature = "packui", ))]
    {
		let rc_path = PathBuf::from("target/resources.rc");
		if !cfg!(target_arch = "arm") {
			pack_ui_archive(std::path::Path::new("src/ui"), &rc_path);
		}
		println!("cargo:rerun-if-changed=src/ui");
    }


    #[cfg(all(windows, feature = "packui"))]
    build_manifest();
    #[cfg(windows)]
    //static_vcruntime::metabuild();
    //#[cfg(windows)]
    build_windows();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target_os == "macos" {
        #[cfg(target_os = "macos")]
        build_mac();
        println!("cargo:rustc-link-lib=framework=ApplicationServices");
        println!("cargo:rustc-link-lib=framework=Vision");
        if std::env::var("CARGO_FEATURE_SCREENCAPTUREKIT").is_ok()
            || std::env::var("CARGO_FEATURE_NATIVECODEC").is_ok()
        {
            for framework in ["ScreenCaptureKit", "UniformTypeIdentifiers"] {
                println!("cargo:rustc-link-arg=-weak_framework");
                println!("cargo:rustc-link-arg={framework}");
            }
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
