//! Embeds two things into the binary at build time.
//! 1. Queries — `runtime/queries/<lang>/*.scm` (so colors work even without the files at run time).
//! 2. Grammars — unpacks `runtime/grammars.tar.gz` (C sources of `bundle = true` in languages.toml, same
//!    commits as the queries) and compiles them in parallel with the C compiler. The default build has only
//!    `core = true` (rust·toml·markdown·comment), `--features bundled-grammars` all of `bundle = true`.
//!    The rest are downloaded at run time.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    queries(&manifest, &out);
    grammars(&manifest, &out);
}

/// Grammar names to embed (`core` from languages.toml `[[grammar]]`, plus `bundle` if the feature is on).
fn wanted(manifest: &Path) -> Vec<String> {
    let path = manifest.join("src/languages.toml");
    println!("cargo:rerun-if-changed={}", path.display());
    let table: toml::Table =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).expect("languages.toml");
    let all = cfg!(feature = "bundled-grammars");
    table["grammar"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| {
            g.get("core").and_then(|v| v.as_bool()) == Some(true)
                || (all && g.get("bundle").and_then(|v| v.as_bool()) == Some(true))
        })
        .map(|g| g["name"].as_str().unwrap().to_string())
        .collect()
}

fn queries(manifest: &Path, out: &Path) {
    let root = manifest.join("runtime/queries");
    // A directory is scanned recursively by cargo — covers every language folder
    println!("cargo:rerun-if-changed={}", root.display());
    let mut entries = Vec::new();
    for lang in std::fs::read_dir(&root).unwrap().flatten() {
        if !lang.path().is_dir() {
            continue;
        }
        for f in std::fs::read_dir(lang.path()).unwrap().flatten() {
            let p = f.path();
            if p.extension().is_some_and(|e| e == "scm") {
                let rel =
                    format!("{}/{}", lang.file_name().to_string_lossy(), f.file_name().to_string_lossy());
                entries.push((rel, p));
            }
        }
    }
    entries.sort();
    let mut s = String::from("pub static QUERIES: &[(&str, &str)] = &[\n");
    for (rel, p) in entries {
        writeln!(s, "    ({rel:?}, include_str!({:?})),", p.display().to_string()).unwrap();
    }
    s.push_str("];\n");
    std::fs::write(out.join("queries.rs"), s).unwrap();
}

fn grammars(manifest: &Path, out: &Path) {
    let archive = manifest.join("runtime/grammars.tar.gz");
    println!("cargo:rerun-if-changed={}", archive.display());
    let dir = out.join("grammars");
    let _ = std::fs::remove_dir_all(&dir);
    let file = std::fs::File::open(&archive).expect("runtime/grammars.tar.gz (cargo xtask vendor-grammars)");
    tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(&dir).expect("unpack grammars");
    let mut names = wanted(manifest);
    for n in &names {
        assert!(dir.join(n).is_dir(), "{n}: not in runtime/grammars.tar.gz (cargo xtask vendor-grammars)");
    }
    names.sort();
    // Compile in parallel (several large parser.c files — slow if done in series)
    let queue = std::sync::Mutex::new(names.clone());
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while let Some(name) = queue.lock().unwrap().pop() {
                    // Unpacked in repo layout — find the src/ holding parser.c (grammars with subfolders)
                    let src = find_src(&dir.join(&name)).expect("parser.c");
                    compile(&src, &name);
                }
            });
        }
    });
    // Name → language function table
    let mut s = String::from("unsafe extern \"C\" {\n");
    for n in &names {
        writeln!(s, "    fn tree_sitter_{}() -> *const ();", n.replace('-', "_")).unwrap();
    }
    s.push_str("}\n\n/// Grammars built into the binary: (grammar name, language function).\npub static BUILTIN_GRAMMARS: &[(&str, unsafe extern \"C\" fn() -> *const ())] = &[\n");
    for n in &names {
        writeln!(s, "    ({n:?}, tree_sitter_{}),", n.replace('-', "_")).unwrap();
    }
    s.push_str("];\n");
    std::fs::write(out.join("builtin_grammars.rs"), s).unwrap();
}

fn find_src(root: &Path) -> Option<PathBuf> {
    if root.join("parser.c").is_file() {
        return Some(root.to_path_buf());
    }
    std::fs::read_dir(root).ok()?.flatten().filter(|e| e.path().is_dir()).find_map(|e| find_src(&e.path()))
}

fn compile(src: &Path, name: &str) {
    // Grammars are optimized regardless of build profile (so parsing isn't slow even in debug builds)
    let mut c = cc::Build::new();
    c.include(src).file(src.join("parser.c")).opt_level(2).warnings(false).cargo_warnings(false);
    if src.join("scanner.c").is_file() {
        c.file(src.join("scanner.c"));
    }
    c.compile(&format!("tree-sitter-{name}"));
    let cc_scanner = ["scanner.cc", "scanner.cpp"].into_iter().map(|f| src.join(f)).find(|p| p.is_file());
    if let Some(scanner) = cc_scanner {
        cc::Build::new()
            .cpp(true)
            .include(src)
            .file(scanner)
            .opt_level(2)
            .warnings(false)
            .cargo_warnings(false)
            .compile(&format!("tree-sitter-{name}-scanner"));
    }
}
