//! tarae runtime — `core` grammars (5 by default, plus the `bundle` ones with `--features bundled-grammars`)
//! are inside the binary (`build.rs`), the other grammars (.so) in
//! `~/.local/share/tarae/grammars/` (`tarae grammar install` fetches and builds them),
//! queries (.scm) inside the binary (`build.rs` embeds `runtime/queries/`). Never reads a Helix install.
//!
//! Lookup order: `$TARAE_RUNTIME` (when developing queries) → `$XDG_DATA_HOME/tarae`
//! (default `~/.local/share/tarae`).
//! A query with the same name under these folders' `queries/` wins; otherwise the built-in one.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/queries.rs"));
include!(concat!(env!("OUT_DIR"), "/builtin_grammars.rs"));

/// Language function of a grammar built into the binary (None if absent — look for a downloaded .so).
pub fn builtin_grammar(name: &str) -> Option<unsafe extern "C" fn() -> *const ()> {
    BUILTIN_GRAMMARS.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

/// Where grammars are downloaded to (`tarae grammar install`).
pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|d| d.join("tarae"))
}

pub fn dirs() -> &'static [PathBuf] {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let mut v: Vec<PathBuf> = Vec::new();
        v.extend(std::env::var_os("TARAE_RUNTIME").map(PathBuf::from));
        v.extend(data_dir());
        v.dedup();
        v
    })
}

/// Finds `rel` in the runtime folders (the first that exists).
pub fn find(rel: impl AsRef<Path>) -> Option<PathBuf> {
    dirs().iter().map(|d| d.join(rel.as_ref())).find(|p| p.exists())
}

/// Query source: `queries/<rel>` in a runtime folder if present, otherwise built-in.
pub fn query(rel: &str) -> Option<String> {
    if let Some(p) = find(format!("queries/{rel}"))
        && let Ok(s) = std::fs::read_to_string(p)
    {
        return Some(s);
    }
    QUERIES.iter().find(|(r, _)| *r == rel).map(|(_, s)| s.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn queries_are_embedded() {
        assert!(super::QUERIES.len() > 100);
        assert!(super::query("rust/highlights.scm").is_some_and(|q| q.contains("@keyword")));
        assert!(super::query("typescript/highlights.scm").is_some_and(|q| q.contains("inherits")));
    }
}
