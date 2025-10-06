use cmake::Config;
use glob::glob;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

macro_rules! debug_log {
    ($($arg:tt)*) => {
        if std::env::var("BUILD_DEBUG").is_ok() {
            println!("cargo:warning=[DEBUG] {}", format!($($arg)*));
        }
    };
}

fn get_cargo_target_dir() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    let profile = std::env::var("PROFILE")?;
    let mut target_dir = None;
    let mut sub_path = out_dir.as_path();
    while let Some(parent) = sub_path.parent() {
        if parent.ends_with(&profile) {
            target_dir = Some(parent);
            break;
        }
        sub_path = parent;
    }
    let target_dir = target_dir.ok_or("not found")?;
    Ok(target_dir.to_path_buf())
}

fn copy_folder(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("Failed to create dst directory");
    if cfg!(unix) {
        std::process::Command::new("cp")
            .arg("-rf")
            .arg(src)
            .arg(dst.parent().unwrap())
            .status()
            .expect("Failed to execute cp command");
    }

    if cfg!(windows) {
        std::process::Command::new("robocopy.exe")
            .arg("/e")
            .arg(src)
            .arg(dst)
            .status()
            .expect("Failed to execute robocopy command");
    }
}

fn extract_lib_names(out_dir: &Path, build_shared_libs: bool) -> Vec<String> {
    let lib_pattern = if cfg!(windows) {
        "*.lib"
    } else if cfg!(target_os = "macos") {
        if build_shared_libs { "*.dylib" } else { "*.a" }
    } else {
        if build_shared_libs { "*.so" } else { "*.a" }
    };
    let libs_dir = out_dir.join("lib*");
    let pattern = libs_dir.join(lib_pattern);
    debug_log!("Extract libs {}", pattern.display());

    let mut lib_names: Vec<String> = Vec::new();

    for entry in glob(pattern.to_str().unwrap()).unwrap() {
        match entry {
            Ok(path) => {
                let stem = path.file_stem().unwrap();
                let stem_str = stem.to_str().unwrap();
                let lib_name = if stem_str.starts_with("lib") {
                    stem_str.strip_prefix("lib").unwrap_or(stem_str)
                } else {
                    stem_str
                };
                lib_names.push(lib_name.to_string());
            }
            Err(e) => println!("cargo:warning=error={}", e),
        }
    }
    lib_names
}

fn extract_lib_assets(out_dir: &Path) -> Vec<PathBuf> {
    let shared_lib_pattern = if cfg!(windows) {
        "*.dll"
    } else if cfg!(target_os = "macos") {
        "*.dylib"
    } else {
        "*.so"
    };

    let shared_libs_dir = if cfg!(windows) { "bin" } else { "lib" };
    let libs_dir = out_dir.join(shared_libs_dir);
    let pattern = libs_dir.join(shared_lib_pattern);
    debug_log!("Extract lib assets {}", pattern.display());
    let mut files = Vec::new();

    for entry in glob(pattern.to_str().unwrap()).unwrap() {
        match entry {
            Ok(path) => files.push(path),
            Err(e) => eprintln!("cargo:warning=error={}", e),
        }
    }

    files
}

fn macos_link_search_path() -> Option<String> {
    let output = Command::new("clang")
        .arg("--print-search-dirs")
        .output()
        .ok()?;
    if !output.status.success() {
        println!("failed to run 'clang --print-search-dirs', continuing without a link search path");
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("libraries: =") {
            let path = line.split('=').nth(1)?;
            return Some(format!("{}/lib/darwin", path));
        }
    }

    println!("failed to determine link search path, continuing without it");
    None
}

fn which_in_path(bin: &str) -> Option<String> {
    let paths = env::var_os("PATH")?;
    for p in std::env::split_paths(&paths) {
        let cand = p.join(bin);
        if cand.exists() && cand.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(md) = fs::metadata(&cand) {
                    if md.permissions().mode() & 0o111 != 0 {
                        return Some(cand.to_string_lossy().into_owned());
                    }
                }
            }
            #[cfg(not(unix))]
            {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    None
}

fn find_glslc_path() -> Option<String> {
    // 1) ANDROID_NDK/shader-tools/*/*/glslc
    if let Ok(ndk) = env::var("ANDROID_NDK") {
        let pat = format!("{ndk}/shader-tools/*/glslc");
        if let Ok(mut it) = glob(&pat) {
            if let Some(Ok(p)) = it.next() {
                return Some(p.to_string_lossy().into_owned());
            }
        }
    }
    // 2) PATH
    which_in_path("glslc")
}

fn main() {
    println!("cargo:rerun-if-env-changed=ANDROID_NDK");
    println!("cargo:rerun-if-env-changed=VULKAN_SDK");
    println!("cargo:rerun-if-env-changed=GGML_VULKAN_COOPMAT_GLSLC_SUPPORT");
    println!("cargo:rerun-if-env-changed=GGML_VULKAN_COOPMAT2_GLSLC_SUPPORT");

    let target = env::var("TARGET").unwrap();
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let target_dir = get_cargo_target_dir().unwrap();
    let llama_dst = out_dir.join("llama.cpp");
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("Failed to get CARGO_MANIFEST_DIR");
    let llama_src = Path::new(&manifest_dir).join("llama.cpp");
    let build_shared_libs_env = false; /* std::env::var("LLAMA_BUILD_SHARED_LIBS")
        .map(|v| v == "1")
        .unwrap_or(false); */
    let build_shared_libs = false; //build_shared_libs_env || cfg!(feature = "cuda") || cfg!(feature = "dynamic-link");

    let profile = env::var("LLAMA_LIB_PROFILE").unwrap_or("Release".to_string());
    let static_crt = env::var("LLAMA_STATIC_CRT").map(|v| v == "1").unwrap_or(false);

    debug_log!("TARGET: {}", target);
    debug_log!("CARGO_MANIFEST_DIR: {}", manifest_dir);
    debug_log!("TARGET_DIR: {}", target_dir.display());
    debug_log!("OUT_DIR: {}", out_dir.display());
    debug_log!("BUILD_SHARED: {}", build_shared_libs);

    if !llama_dst.exists() {
        debug_log!("Copy {} to {}", llama_src.display(), llama_dst.display());
        copy_folder(&llama_src, &llama_dst);
        // Убираем вложенный git-артефакт (в OUT_DIR он невалиден и ломает вызовы git)
        let git_path = llama_dst.join(".git");
        if git_path.exists() {
            let _ = std::fs::remove_file(&git_path)
                .or_else(|_| std::fs::remove_dir_all(&git_path));
        }
    }

    unsafe {
        env::set_var(
            "CMAKE_BUILD_PARALLEL_LEVEL",
            std::thread::available_parallelism().unwrap().get().to_string(),
        )
    };

    if cfg!(all(feature = "mpi", target_os = "macos")) {
        unsafe { env::set_var("CC", "/opt/homebrew/bin/mpicc") };
        unsafe { env::set_var("CXX", "/opt/homebrew/bin/mpicxx") };
    }

    // --- macOS: очистка протёкших NDK/инклудов и поиск SDK ---
    if target.contains("apple") {
        for k in [
            "BINDGEN_EXTRA_CLANG_ARGS",
            "BINDGEN_EXTRA_CLANG_ARGS_aarch64-apple-darwin",
            "BINDGEN_EXTRA_CLANG_ARGS_aarch64_apple_darwin",
            "CPATH","C_INCLUDE_PATH","CPLUS_INCLUDE_PATH","CPPFLAGS","CFLAGS",
            "ANDROID_NDK","ANDROID_NDK_HOME","ANDROID_HOME",
        ] {
            std::env::remove_var(k);
        }
    }
    let sdkroot = if target.contains("apple") {
        let out = std::process::Command::new("xcrun")
            .args(["--sdk","macosx","--show-sdk-path"])
            .output()
            .expect("xcrun not found; install Xcode Command Line Tools");
        let s = String::from_utf8(out.stdout).unwrap();
        Some(s.trim().to_string())
    } else { None };

    // Bindings
    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .generate_comments(true)
        // macOS: иногда падает на <string> — просим C++
        .clang_arg("-xc++")
        .clang_arg("-std=c++11")
        .clang_arg(format!("-I{}", llama_dst.join("include").display()))
        .clang_arg(format!("-I{}", llama_dst.join("ggml/include").display()))
        .clang_arg(format!("-I{}", llama_dst.join("src").display()))
        .clang_arg(format!("-I{}", llama_dst.join("common").display()))
        // направим bindgen в SDK macOS, чтобы не лез в NDK
        .clang_args(
            sdkroot
                .as_ref()
                .map(|p| vec!["-isysroot".into(), p.clone()])
                .unwrap_or_default(),
        )
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .derive_partialeq(true)
        .allowlist_function("ggml_.*")
        .allowlist_type("ggml_.*")
        .allowlist_function("llama_.*")
        .allowlist_function("llama_lora_.*")
        .allowlist_type("llama_.*")
        .allowlist_function("common_token_to_piece")
        .allowlist_function("common_tokenize")
        .allowlist_item("LLAMA_.*")
        .opaque_type("llama_grammar")
        .opaque_type("llama_grammar_parser")
        .opaque_type("llama_sampler_chain")
        .opaque_type("std::.*");

    if cfg!(feature = "rpc") {
        builder = builder
            .clang_arg("-DRPC_SUPPORT")
            .allowlist_function("ggml_backend_rpc_.*")
            .allowlist_type("ggml_backend_rpc_.*");
    }

    let bindings = builder
        .use_core()
        .prepend_enum_name(false)
        .generate()
        .expect("Failed to generate bindings");

    let bindings_path = out_dir.join("bindings.rs");
    bindings
        .write_to_file(&bindings_path)
        .expect("Failed to write bindings");

    // временный фикс: убираем unsafe в extern "C"
    let contents = std::fs::read_to_string(&bindings_path).unwrap();
    let contents = contents.replace("unsafe extern \"C\" {", " extern \"C\" {");
    fs::write(&bindings_path, contents).unwrap();

    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=./sherpa-onnx");

    debug_log!("Bindings Created");

    // Build with CMake
    let mut config = Config::new(&llama_dst);

    config.define("LLAMA_BUILD_TOOLS", "OFF");
    config.define("LLAMA_BUILD_EXAMPLES", "OFF");
    config.define("LLAMA_BUILD_TESTS", "OFF");
    config.define("LLAMA_BUILD_SERVER", "OFF");
    config.define("BUILD_SHARED_LIBS", if build_shared_libs { "ON" } else { "OFF" });

    if cfg!(all(target_os = "windows", target_arch = "arm")) {
        config.define("GGML_OPENMP", "OFF");
    }

    if cfg!(windows) {
        config.static_crt(static_crt);
    }

    if target.contains("android") && target.contains("aarch64") {
        let android_ndk = env::var("ANDROID_NDK")
            .expect("Please install Android NDK and ensure that ANDROID_NDK env variable is set");
        config.define(
            "CMAKE_TOOLCHAIN_FILE",
            format!("{android_ndk}/build/cmake/android.toolchain.cmake"),
        );
        config.define("ANDROID_ABI", "arm64-v8a");
        let min_api = std::env::var("ANDROID_MIN_SDK").unwrap_or_else(|_| "33".into());
        config.define("ANDROID_PLATFORM", format!("android-{}", min_api));
        config.define("CMAKE_SYSTEM_PROCESSOR", "arm64");
        config.define("CMAKE_C_FLAGS", "-march=armv8.7a");
        config.define("CMAKE_CXX_FLAGS", "-march=armv8.7a");
        config.define("GGML_OPENMP", "OFF");
        config.define("GGML_LLAMAFILE", "OFF");
    }

    if cfg!(feature = "vulkan") {
        config.define("GGML_VULKAN", "ON");

        // glslc: возьмём из NDK shader-tools или из PATH
        if let Some(glslc) = find_glslc_path() {
            config.define("Vulkan_GLSLC_EXECUTABLE", &glslc);
            config.define("GGML_VULKAN_GLSLC", &glslc);
            debug_log!("Using glslc at {}", glslc);
        } else {
            panic!("glslc not found (NDK shader-tools or PATH)");
        }

        // Заголовки Vulkan-Hpp (vulkan.hpp) из LunarG SDK
        if let Ok(vsdk) = env::var("VULKAN_SDK") {
            let inc = format!("{vsdk}/include");
            config.define("Vulkan_INCLUDE_DIR", &inc);
            debug_log!("Vulkan_INCLUDE_DIR = {}", inc);
        }

        if target.contains("android") {
            println!("cargo:rustc-link-lib=vulkan");
        }

        // Опционально: отключить автодетект cooperative matrices через окружение
        if let Ok(v) = env::var("GGML_VULKAN_COOPMAT_GLSLC_SUPPORT") {
            config.define("GGML_VULKAN_COOPMAT_GLSLC_SUPPORT", v);
        }
        if let Ok(v) = env::var("GGML_VULKAN_COOPMAT2_GLSLC_SUPPORT") {
            config.define("GGML_VULKAN_COOPMAT2_GLSLC_SUPPORT", v);
        }

        // Windows / Linux линковка по-прежнему хинтится ниже
        if cfg!(windows) {
            let vulkan_path = env::var("VULKAN_SDK")
                .expect("Please install Vulkan SDK and ensure that VULKAN_SDK env variable is set");
            let vulkan_lib_path = Path::new(&vulkan_path).join("Lib");
            println!("cargo:rustc-link-search={}", vulkan_lib_path.display());
            println!("cargo:rustc-link-lib=vulkan-1");
        }
        if cfg!(target_os = "linux") && !target.contains("android") {
            println!("cargo:rustc-link-lib=vulkan");
        }
    }

    if cfg!(feature = "cuda") {
        config.define("GGML_CUDA", "ON");
    }

    if cfg!(feature = "openmp") {
        config.define("GGML_OPENMP", "ON");
    } else {
        config.define("GGML_OPENMP", "OFF");
    }

    if cfg!(all(feature = "mpi")) {
        config.define("LLAMA_MPI", "ON");
    }

    if cfg!(feature = "rpc") {
        config.define("GGML_RPC", "ON");
    }

    // macOS: пробросим SDK и архитектуру
    if let Some(sdk) = &sdkroot {
        config.define("CMAKE_OSX_SYSROOT", sdk);
        config.define("CMAKE_OSX_ARCHITECTURES", "arm64");
    }

    config
        .profile(&profile)
        .very_verbose(std::env::var("CMAKE_VERBOSE").is_ok())
        // форсим полную реконфигурацию, чтобы не зависать на "Skipping configuration step"
        .always_configure(true);

    if cfg!(feature = "curl") {
        config.define("LLAMA_CURL", "ON");
    } else {
        config.define("LLAMA_CURL", "OFF");
    }


    let build_dir = config.build();

    // Search paths
    println!("cargo:rustc-link-search={}", out_dir.join("lib").display());
    println!("cargo:rustc-link-search={}", out_dir.join("lib64").display());
    println!("cargo:rustc-link-search={}", build_dir.display());

    // Link libraries
    let llama_libs_kind = if build_shared_libs { "dylib" } else { "static" };
    let llama_libs = extract_lib_names(&out_dir, build_shared_libs);
    assert_ne!(llama_libs.len(), 0);

    for lib in llama_libs {
        debug_log!("LINK {}", format!("cargo:rustc-link-lib={}={}", llama_libs_kind, lib));
        println!("{}", format!("cargo:rustc-link-lib={}={}", llama_libs_kind, lib));
    }

    // OpenMP
    if cfg!(feature = "openmp") {
        if target.contains("gnu") {
            println!("cargo:rustc-link-lib=gomp");
        }
    }

    // Windows debug
    if cfg!(all(debug_assertions, windows)) {
        println!("cargo:rustc-link-lib=dylib=msvcrtd");
    }

    if target.contains("apple") {
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=Metal");
        println!("cargo:rustc-link-lib=framework=MetalKit");
        println!("cargo:rustc-link-lib=framework=Accelerate");
        println!("cargo:rustc-link-lib=c++");
    } else if target.contains("android") {
        println!("cargo:rustc-link-lib=c++_static");
        println!("cargo:rustc-link-lib=atomic");
    } else if target.contains("linux") && !target.contains("android") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }

    if target.contains("apple") {
        if let Some(path) = macos_link_search_path() {
            println!("cargo:rustc-link-lib=clang_rt.osx");
            println!("cargo:rustc-link-search={}", path);
        }
    }

    // copy DLLs to target
    if build_shared_libs {
        let libs_assets = extract_lib_assets(&out_dir);
        for asset in libs_assets {
            let filename = asset.file_name().unwrap().to_str().unwrap();
            let dst = target_dir.join(filename);
            debug_log!("HARD LINK {} TO {}", asset.display(), dst.display());
            if !dst.exists() {
                std::fs::hard_link(asset.clone(), dst).unwrap();
            }

            if target_dir.join("examples").exists() {
                let dst = target_dir.join("examples").join(filename);
                debug_log!("HARD LINK {} TO {}", asset.display(), dst.display());
                if !dst.exists() {
                    std::fs::hard_link(asset.clone(), dst).unwrap();
                }
            }

            let dst = target_dir.join("deps").join(filename);
            debug_log!("HARD LINK {} TO {}", asset.display(), dst.display());
            if !dst.exists() {
                std::fs::hard_link(asset.clone(), dst).unwrap();
            }
        }
    }
}
