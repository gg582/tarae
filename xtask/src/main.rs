//! tarae development tasks — `cargo xtask <task>`. Not part of the user binary.
//!
//! - `vendor-grammars` — fetches `bundle = true` entries of `languages.toml` `[[grammar]]` at their commits
//!   (reuses what's in `~/.local/share/tarae/sources` — same place and marker as `tarae grammar install`)
//!   and packs `src/` parser.c·scanner.c|cc|cpp·*.h plus the upstream LICENSE into `runtime/grammars.tar.gz`.
//!   Also follows
//!   `#include "../…"` pointing outside `src/` (typescript's common/scanner.h etc.), kept at their repo paths
//!   (`<name>/<subfolder>/src/…` — so relative paths still match). Times and owners zeroed — same input,
//!   same file.
//!   Run only when grammar commits or the bundle list change.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

type Result<T> = std::result::Result<T, String>;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("vendor-grammars") => vendor_grammars(),
        _ => Err("usage: cargo xtask vendor-grammars".into()),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

struct Grammar {
    name: String,
    git: String,
    rev: String,
    subpath: Option<String>,
}

fn bundled_grammars() -> Result<Vec<Grammar>> {
    let path = root().join("src/languages.toml");
    let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let table: toml::Table = toml::from_str(&src).map_err(|e| format!("languages.toml: {e}"))?;
    let str_of = |g: &toml::Value, k: &str| g.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let mut out: Vec<Grammar> = table
        .get("grammar")
        .and_then(|g| g.as_array())
        .ok_or("languages.toml: no [[grammar]]")?
        .iter()
        .filter(|g| g.get("bundle").and_then(|b| b.as_bool()) == Some(true))
        .map(|g| {
            Ok(Grammar {
                name: str_of(g, "name").ok_or("grammar without name")?,
                git: str_of(g, "git").ok_or("grammar without git")?,
                rev: str_of(g, "rev").ok_or("grammar without rev")?,
                subpath: str_of(g, "subpath"),
            })
        })
        .collect::<Result<_>>()?;
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn cache_dir() -> Result<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or("no home directory")?;
    Ok(data.join("tarae/sources"))
}

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Checks out the repo at that commit (as-is if already there) — returns the repo root.
fn checkout(g: &Grammar) -> Result<PathBuf> {
    let repo = g.git.trim_end_matches('/').rsplit('/').next().unwrap_or("repo");
    let dir = cache_dir()?.join(format!("{repo}-{}", &g.rev[..g.rev.len().min(12)]));
    let stamp = dir.join(".tarae-rev");
    if std::fs::read_to_string(&stamp).is_ok_and(|r| r.trim() == g.rev) {
        return Ok(dir);
    }
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    git(&dir, &["init", "-q"])?;
    if git(&dir, &["fetch", "-q", "--depth", "1", &g.git, &g.rev])
        .and_then(|_| git(&dir, &["checkout", "-q", "FETCH_HEAD"]))
        .is_err()
    {
        git(&dir, &["fetch", "-q", &g.git])?;
        git(&dir, &["checkout", "-q", &g.rev])?;
    }
    std::fs::write(&stamp, &g.rev).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// `a/b/../c` → `a/c` (without touching the file system).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

/// Follows `#include "…"` and adds files inside the repo.
fn follow_includes(file: &Path, repo: &Path, files: &mut BTreeSet<PathBuf>) {
    let Ok(bytes) = std::fs::read(file) else { return };
    for line in String::from_utf8_lossy(&bytes).lines() {
        let line = line.trim();
        let Some(rel) = line.strip_prefix("#include").and_then(|r| r.split('"').nth(1)) else { continue };
        let f = normalize(&file.parent().unwrap().join(rel));
        if f.is_file() && f.starts_with(repo) && files.insert(f.clone()) {
            follow_includes(&f, repo, files);
        }
    }
}

fn walk(dir: &Path, out: &mut BTreeSet<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| ["c", "cc", "cpp", "h"].iter().any(|e| x == *e)) {
            out.insert(p);
        }
    }
}

/// `LICENSE`, `LICENSE.md`, `COPYING` … directly in `dir`.
fn license_file(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_uppercase();
            p.is_file()
                && (name.starts_with("LICENSE") || name.starts_with("LICENCE") || name.starts_with("COPYING"))
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

fn vendor_grammars() -> Result<()> {
    let grammars = bundled_grammars()?;
    // Fetch in parallel (usually already downloaded)
    let repos: Vec<Result<PathBuf>> = std::thread::scope(|s| {
        grammars
            .iter()
            .map(|g| s.spawn(|| checkout(g)))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect()
    });
    let mut tar =
        tar::Builder::new(flate2::GzBuilder::new().mtime(0).write(Vec::new(), flate2::Compression::best()));
    tar.mode(tar::HeaderMode::Deterministic);
    for (g, repo) in grammars.iter().zip(repos) {
        let repo = repo.map_err(|e| format!("{}: {e}", g.name))?;
        let base = g.subpath.as_ref().map_or(repo.clone(), |p| repo.join(p));
        let mut files = BTreeSet::new();
        walk(&base.join("src"), &mut files);
        for f in files.clone() {
            follow_includes(&f, &repo, &mut files);
        }
        if !files.iter().any(|f| f.ends_with("parser.c")) {
            return Err(format!("{}: no parser.c under {}", g.name, base.display()));
        }
        // Upstream license travels with the sources (MIT etc. require the notice)
        let license = [&base, &repo].into_iter().find_map(|d| license_file(d));
        let Some(license) = license else { return Err(format!("{}: no LICENSE file", g.name)) };
        files.insert(license);
        for f in &files {
            let arc = Path::new(&g.name).join(f.strip_prefix(&repo).unwrap());
            let data = std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?;
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_mtime(0);
            tar.append_data(&mut h, &arc, data.as_slice()).map_err(|e| e.to_string())?;
        }
        eprintln!("  {:16} {} files", g.name, files.len());
    }
    let gz = tar.into_inner().and_then(|z| z.finish()).map_err(|e| e.to_string())?;
    let out = root().join("runtime/grammars.tar.gz");
    std::fs::File::create(&out)
        .and_then(|mut f| f.write_all(&gz))
        .map_err(|e| format!("{}: {e}", out.display()))?;
    eprintln!("{} grammars → {} ({:.1} MB)", grammars.len(), out.display(), gz.len() as f64 / 1e6);
    Ok(())
}
