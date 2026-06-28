use cmake::Config;
use glob::glob;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

// ===================== helpers =====================
macro_rules! debug_log {
    ($($arg:tt)*) => {
        if std::env::var("BUILD_DEBUG").is_ok() {
            println!("cargo:warning=[DEBUG] {}", format!($($arg)*));
        }
    };
}

fn target_flags() -> (bool, bool, bool, bool, bool, bool, String) {
    let target = env::var("TARGET").expect("TARGET not set");
    let is_android = target.contains("android");
    let is_apple = target.contains("apple");
    let is_ios = target.contains("apple-ios");
    let is_macos = target.contains("apple-darwin");
    let is_windows = target.contains("windows");
    let is_linux = target.contains("linux") && !is_android;
    (is_android, is_apple, is_ios, is_macos, is_windows, is_linux, target)
}

fn apple_sdk_name_for_target(target: &str) -> &'static str {
    if target.contains("apple-ios") {
        if target.contains("sim") { "iphonesimulator" } else { "iphoneos" }
    } else {
        "macosx"
    }
}

fn xcrun_sdk_path(sdk: &str) -> String {
    let out = Command::new("xcrun")
        .args(["--sdk", sdk, "--show-sdk-path"])
        .output()
        .expect("xcrun not found; install Xcode Command Line Tools");
    assert!(out.status.success(), "xcrun failed to get SDK path for {sdk}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn which_in_path(bin: &str) -> Option<PathBuf> {
    let Some(paths) = env::var_os("PATH") else { return None; };
    #[cfg(windows)]
    let exts: Vec<String> = env::var("PATHEXT").unwrap_or(".EXE;.BAT;.CMD".into())
        .split(';').map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase()).map(|s| format!({{".{}"}}, s)).collect();
    #[cfg(not(windows))]
    let exts: Vec<String> = vec![String::new()];

    for dir in env::split_paths(&paths) {
        for ext in &exts {
            let candidate = if ext.is_empty() { dir.join(bin) } else { dir.join(format!("{}{}", bin, ext)) };
            if candidate.is_file() { return Some(candidate); }
        }
    }
    None
}

fn find_glslc_path() -> Option<PathBuf> {
    // 1) ANDROID_NDK shader-tools (2-level glob because NDK layout varies)
    if let Ok(ndk) = env::var("ANDROID_NDK") {
        for pat in [
            format!("{ndk}/shader-tools/*/*/glslc"),
            format!("{ndk}/shader-tools/*/glslc"),
            format!("{ndk}/toolchains/llvm/prebuilt/*/bin/glslc"),
        ] {
            if let Ok(mut it) = glob(&pat) { if let Some(Ok(p)) = it.next() { return Some(p); } }
        }
    }
    // 2) VULKAN_SDK
    if let Ok(vsdk) = env::var("VULKAN_SDK") {
        for cand in ["glslc", "bin/glslc", "Bin/glslc", "Bin/glslc.exe"] {
            let p = Path::new(&vsdk).join(cand);
            if p.is_file() { return Some(p); }
        }
    }
    // 3) PATH
    which_in_path("glslc")
}

fn collect_lib_names(search_dirs: &[PathBuf], static_only: bool, is_windows: bool) -> Vec<String> {
    use std::collections::BTreeSet;

    let mut out: BTreeSet<String> = BTreeSet::new();

    // Паттерны по платформам:
    // - Windows: линкуем по *.lib (и для статик, и для импорт-либов). *.dll не используем для -l.
    // - Unix/Apple: статик — lib*.a; шары — lib*.so и/или lib*.dylib.
    let static_patterns: &[&str] = if is_windows { &["*.lib"] } else { &["lib*.a"] };
    let shared_patterns: &[&str] = if is_windows {
        // импорт-либы на Windows всё равно *.lib; отдельный проход по *.dll не нужен.
        &["*.lib"]
    } else {
        // ищем оба варианта, так корректно для Linux/macOS без знания таргета
        &["lib*.so", "lib*.dylib"]
    };

    for dir in search_dirs {
        // Статические библиотеки
        for pat in static_patterns {
            let pattern = dir.join(pat);
            if let Some(s) = pattern.to_str() {
                if let Ok(entries) = glob(s) {
                    for entry in entries {
                        if let Ok(path) = entry {
                            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                                let name = if is_windows {
                                    // foo.lib -> foo
                                    stem.to_string()
                                } else {
                                    // libfoo.a -> foo
                                    stem.trim_start_matches("lib").to_string()
                                };
                                out.insert(name);
                            }
                        }
                    }
                }
            }
        }

        // Динамические (только если разрешено)
        if !static_only {
            for pat in shared_patterns {
                let pattern = dir.join(pat);
                if let Some(s) = pattern.to_str() {
                    if let Ok(entries) = glob(s) {
                        for entry in entries {
                            if let Ok(path) = entry {
                                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                                    let name = if is_windows {
                                        // импорт-либы *.lib уже покрыты выше; dups отсеются BTreeSet'ом
                                        stem.to_string()
                                    } else {
                                        stem.trim_start_matches("lib").to_string()
                                    };
                                    out.insert(name);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Отсортированный список (BTreeSet) -> Vec
    out.into_iter().collect()
}


// ===================== main =====================
fn main() {
    // Re-run hints
    println!("cargo:rerun-if-env-changed=ANDROID_NDK");
    println!("cargo:rerun-if-env-changed=ANDROID_MIN_SDK");
    println!("cargo:rerun-if-env-changed=VULKAN_SDK");
    println!("cargo:rerun-if-env-changed=IOS_DEPLOYMENT_TARGET");
    println!("cargo:rerun-if-env-changed=LLAMA_STATIC_CRT");
    println!("cargo:rerun-if-env-changed=LLAMA_LIB_PROFILE");

    let (is_android, is_apple, is_ios, is_macos, is_windows, is_linux, target) = target_flags();

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let src_dir = manifest_dir.join("llama.cpp"); // предполагается сабмодуль/вендор

    let build_profile = env::var("LLAMA_LIB_PROFILE").unwrap_or_else(|_| "Release".to_string());
    let static_crt = env::var("LLAMA_STATIC_CRT").map(|v| v == "1").unwrap_or(false);

    let feature_vulkan = cfg!(feature = "vulkan");
    let feature_metal  = cfg!(feature = "metal"); // для Apple
    let build_shared_libs = false; // статически надёжнее

    debug_log!("TARGET = {}", target);
    debug_log!("OUT_DIR = {}", out_dir.display());
    debug_log!("SRC = {}", src_dir.display());
    debug_log!("PROFILE = {}", build_profile);
    debug_log!("FEATURES: metal={} vulkan={}", feature_metal, feature_vulkan);

    // -------- bindgen (заголовки) --------
    // опционально — если проекту нужны биндинги из wrapper.h
    if Path::new("wrapper.h").exists() {
        // Apple SDK for clang/headers
        let (sdk_name, sdk_path) = if is_apple {
            let name = apple_sdk_name_for_target(&target);
            (name.to_string(), xcrun_sdk_path(name))
        } else { (String::new(), String::new()) };

        let mut builder = bindgen::Builder::default()
            .header("wrapper.h")
            .generate_comments(true)
            .clang_arg("-xc++")
            .clang_arg("-std=c++11")
            .clang_arg(format!("-I{}", src_dir.join("include").display()))
            .clang_arg(format!("-I{}", src_dir.join("ggml/include").display()))
            .clang_arg(format!("-I{}", src_dir.join("src").display()))
            .clang_arg(format!("-I{}", src_dir.join("common").display()))
            .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
            .derive_partialeq(true)
            .allowlist_function("ggml_.*")
            .allowlist_type("ggml_.*")
            .allowlist_function("llama_.*")
            .allowlist_type("llama_.*")
            .allowlist_item("LLAMA_.*")
            .opaque_type("llama_grammar")
            .opaque_type("llama_grammar_parser")
            .opaque_type("llama_sampler_chain")
            .opaque_type("std::.*");

        if !sdk_path.is_empty() {
            builder = builder.clang_args(["-isysroot", &sdk_path]);
        }

        let bindings = builder.use_core().prepend_enum_name(false)
            .generate().expect("bindgen failed");
        let bindings_path = out_dir.join("bindings.rs");
        bindings.write_to_file(&bindings_path).expect("write bindings");
        println!("cargo:rerun-if-changed=wrapper.h");
        debug_log!("Bindings generated at {}", bindings_path.display());
    }

    // -------- CMake configure/build --------
    let mut cfg = Config::new(&src_dir);
    cfg.profile(&build_profile)
        .very_verbose(env::var("CMAKE_VERBOSE").is_ok())
        .always_configure(true);

    // core switches
    cfg.define("BUILD_SHARED_LIBS", if build_shared_libs { "ON" } else { "OFF" });
    cfg.define("LLAMA_BUILD_TESTS", "OFF");
    cfg.define("LLAMA_BUILD_EXAMPLES", "OFF");
    cfg.define("LLAMA_BUILD_TOOLS", "OFF");
    cfg.define("LLAMA_BUILD_SERVER", "OFF");

    // math backends common
    if is_apple { cfg.define("GGML_USE_ACCELERATE", "ON"); } else { cfg.define("GGML_USE_ACCELERATE", "OFF"); }

    // Metal (Apple): controlled by feature "metal"
    if is_apple {
        if feature_metal { cfg.define("GGML_METAL", "ON"); } else { cfg.define("GGML_METAL", "OFF"); }
        cfg.define("GGML_BLAS", "OFF");

        // Toolchain hints for Apple platforms
        let sdk_name = apple_sdk_name_for_target(&target);
        let sdk_path = xcrun_sdk_path(sdk_name);
        cfg.define("CMAKE_OSX_SYSROOT", &sdk_path);

        // architectures
        if target.contains("x86_64") { cfg.define("CMAKE_OSX_ARCHITECTURES", "x86_64"); }
        else { cfg.define("CMAKE_OSX_ARCHITECTURES", "arm64"); }

        if is_ios {
            cfg.define("CMAKE_SYSTEM_NAME", "iOS");
            let ios_min = env::var("IOS_DEPLOYMENT_TARGET").unwrap_or_else(|_| "13.0".to_string());
            cfg.define("CMAKE_OSX_DEPLOYMENT_TARGET", &ios_min);
            // Не задаём вручную -isysroot в CFLAGS/CXXFLAGS — CMake сделает сам через CMAKE_OSX_SYSROOT
            // PIC обязателен для iOS
            cfg.define("CMAKE_POSITION_INDEPENDENT_CODE", "ON");
        }
    }

    // Vulkan backend
    if feature_vulkan {
        cfg.define("GGML_VULKAN", "ON");
        if let Some(glslc) = find_glslc_path() {
            cfg.define("Vulkan_GLSLC_EXECUTABLE", glslc.to_str().unwrap());
            cfg.define("GGML_VULKAN_GLSLC", glslc.to_str().unwrap());
            debug_log!("Using glslc = {}", glslc.display());
        } else {
            panic!("glslc not found (NDK shader-tools, VULKAN_SDK, or PATH)");
        }
        if let Ok(vsdk) = env::var("VULKAN_SDK") { cfg.define("Vulkan_INCLUDE_DIR", format!("{vsdk}/include")); }
        if is_android { println!("cargo:rustc-link-lib=vulkan"); }
        if is_windows {
            let vsdk = env::var("VULKAN_SDK").expect("VULKAN_SDK must be set on Windows for Vulkan");
            println!("cargo:rustc-link-search={}", Path::new(&vsdk).join("Lib").display());
            println!("cargo:rustc-link-lib=vulkan-1");
        }
        if is_linux { println!("cargo:rustc-link-lib=vulkan"); }
    } else {
        cfg.define("GGML_VULKAN", "OFF");
    }

    // Windows CRT
    if is_windows && target.contains("msvc") {
        cfg.static_crt(static_crt);
    }

    // Android toolchain
    if is_android {
        let ndk = env::var("ANDROID_NDK").expect("ANDROID_NDK must be set for Android builds");
        cfg.define("CMAKE_TOOLCHAIN_FILE", format!("{ndk}/build/cmake/android.toolchain.cmake"));
        cfg.define("ANDROID_ABI", if target.contains("aarch64") { "arm64-v8a" } else if target.contains("armv7") { "armeabi-v7a" } else if target.contains("i686") { "x86" } else { "x86_64" });
        let min_api = env::var("ANDROID_MIN_SDK").unwrap_or_else(|_| "28".into());
        cfg.define("ANDROID_PLATFORM", format!("android-{min_api}"));
        cfg.define("CMAKE_SYSTEM_PROCESSOR", if target.contains("aarch64") { "arm64" } else { "arm" });
        // Не форсим -march, даём NDK подобрать безопасно
        cfg.define("GGML_OPENMP", "OFF");
        cfg.define("GGML_LLAMAFILE", "OFF");
    }

    // Build
    let build_dir = cfg.build();

    // -------- Link search paths --------
    for p in [
        out_dir.join("lib"),
        out_dir.join("lib64"),
        build_dir.clone(),
        build_dir.join("lib"),
        build_dir.join("Release"),
        build_dir.join("Debug"),
    ] {
        if p.is_dir() { println!("cargo:rustc-link-search={}", p.display()); }
    }

    // -------- Link libraries (static preferred) --------
    let search_dirs = vec![
        out_dir.join("lib"), out_dir.join("lib64"), build_dir.clone(), build_dir.join("lib"),
        build_dir.join("Release"), build_dir.join("Debug")
    ];
    let libs = collect_lib_names(&search_dirs, true, is_windows);
    assert!(!libs.is_empty(), "no static libraries produced by CMake: searched in {:?}", search_dirs);

    for lib in libs { println!("cargo:rustc-link-lib=static={}", lib); }

    // -------- Platform link hints --------
    if is_apple {
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=Accelerate");
        println!("cargo:rustc-link-lib=framework=Metal");
        println!("cargo:rustc-link-lib=framework=MetalKit");
        println!("cargo:rustc-link-lib=c++");
    } else if is_android {
        println!("cargo:rustc-link-lib=c++_static");
        println!("cargo:rustc-link-lib=atomic");
    } else if is_linux {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }
}
