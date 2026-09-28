//! Syntax trees·highlighting — tree-sitter.
//!
//! - Grammars: built into the binary (`build.rs` — the default build has the 5 `core` ones, `bundled-grammars`
//!   adds the `bundle` ones), the rest are downloaded shared libraries dlopen'd **on demand**.
//! - Queries in Helix format as is (`; inherits: a,b` supported). When several patterns match the same
//!   node **the later pattern wins** (a premise of Helix 25.07 queries — the generic
//!   `(identifier) @variable` is at the top of the file).
//! - Parsing runs in the background (principle 4). Edits are applied to the existing tree at once via
//!   `edit`, so highlights follow the text even while parsing. Edits arriving mid-parse are collected
//!   and reapplied to the new tree.
//! - Highlighting runs queries only over the visible line range.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use ropey::Rope;
use tree_sitter::{InputEdit, Language, Node, Parser, Point, Query, QueryCursor, StreamingIterator, Tree};

use crate::runtime;

// ── Language table ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LangSpec {
    pub name: String,
    pub grammar: String,
    extensions: Vec<String>,
    filenames: Vec<String>,
    path_suffixes: Vec<String>,
    /// Path-suffix patterns — up to one `*` per segment (`templates/*.yaml` = Helm chart templates).
    globs: Vec<String>,
    /// Line comment token (`//`) · block comment (`/*`, `*/`) — `C-c` toggles them.
    pub comment_token: Option<String>,
    pub block_comment: Option<(String, String)>,
    /// Insert-mode pairs (open, close) — None = the defaults.
    pub auto_pairs: Option<Vec<(char, char)>>,
}

/// Insert-mode pairs when the language doesn't say.
pub const DEFAULT_PAIRS: &[(char, char)] =
    &[('(', ')'), ('[', ']'), ('{', '}'), ('"', '"'), ('\'', '\''), ('`', '`')];

fn specs() -> &'static [LangSpec] {
    static SPECS: OnceLock<Vec<LangSpec>> = OnceLock::new();
    SPECS.get_or_init(|| {
        let table: toml::Table =
            toml::from_str(include_str!("languages.toml")).expect("languages.toml parses");
        let strings = |v: Option<&toml::Value>| -> Vec<String> {
            v.and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        table["language"]
            .as_array()
            .expect("[[language]]")
            .iter()
            .filter_map(|l| {
                let name = l.get("name")?.as_str()?.to_string();
                Some(LangSpec {
                    grammar: l.get("grammar").and_then(|g| g.as_str()).unwrap_or(&name).to_string(),
                    extensions: strings(l.get("extensions")),
                    filenames: strings(l.get("filenames")),
                    path_suffixes: strings(l.get("path-suffixes")),
                    globs: strings(l.get("globs")),
                    comment_token: l.get("comment-token").and_then(|t| t.as_str()).map(str::to_string),
                    block_comment: l.get("block-comment-tokens").and_then(|b| {
                        Some((b.get("start")?.as_str()?.to_string(), b.get("end")?.as_str()?.to_string()))
                    }),
                    auto_pairs: l.get("auto-pairs").map(|_| {
                        strings(l.get("auto-pairs"))
                            .iter()
                            .filter_map(|p| {
                                let mut c = p.chars();
                                Some((c.next()?, c.next()?))
                            })
                            .collect()
                    }),
                    name,
                })
            })
            .collect()
    })
}

/// Up to one `*` per segment (`_*.tpl`, `*.yaml`).
fn wildcard(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        Some((a, b)) => s.len() >= a.len() + b.len() && s.starts_with(a) && s.ends_with(b),
        None => pat == s,
    }
}

/// Whether the trailing segments of a path match the pattern (`templates/*.yaml`).
fn glob_match(pat: &str, path: &Path) -> bool {
    let parts: Vec<String> =
        path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let pats: Vec<&str> = pat.split('/').collect();
    parts.len() >= pats.len() && pats.iter().rev().zip(parts.iter().rev()).all(|(p, s)| wildcard(p, s))
}

/// Picks a language by path: path suffix → path pattern → exact file name → extension.
pub fn detect(path: &Path) -> Option<&'static LangSpec> {
    let full = path.to_string_lossy();
    let file = path.file_name()?.to_string_lossy();
    let ext = path.extension().map(|e| e.to_string_lossy());
    specs()
        .iter()
        .find(|s| s.path_suffixes.iter().any(|p| full.ends_with(p.as_str())))
        .or_else(|| specs().iter().find(|s| s.globs.iter().any(|g| glob_match(g, path))))
        .or_else(|| specs().iter().find(|s| s.filenames.iter().any(|f| *f == file)))
        .or_else(|| {
            let ext = ext?;
            specs().iter().find(|s| s.extensions.iter().any(|e| *e == ext))
        })
}

/// Known language names (`:lang` completion).
pub fn names() -> impl Iterator<Item = &'static str> {
    specs().iter().map(|s| s.name.as_str())
}

pub fn spec(name: &str) -> Option<&'static LangSpec> {
    specs().iter().find(|s| s.name == name)
}

/// Picks a language by a human-written name (code block header `rust`·`rs`·`sh`·`py` …, shebang `python3`).
pub fn resolve(name: &str) -> Option<&'static LangSpec> {
    let n = name.trim().trim_start_matches('{').trim_start_matches('.').to_ascii_lowercase();
    let n = match n.as_str() {
        "sh" | "shell" | "zsh" | "console" | "shellsession" => "bash",
        "golang" => "go",
        "c++" | "cxx" => "cpp",
        "py" | "python3" | "py3" => "python",
        "node" | "js" | "mjs" | "cjs" => "javascript",
        "yml" => "yaml",
        "jsonc" | "json5" => "json",
        "cs" | "csharp" => "c-sharp",
        "kt" => "kotlin",
        "hs" => "haskell",
        "rb" => "ruby",
        "text" | "txt" | "plain" | "plaintext" | "" => return None,
        x => x,
    };
    spec(n).or_else(|| detect(Path::new(&format!("x.{n}"))))
}

// ── Global capture names ────────────────────────────────────────────────

#[derive(Default)]
struct Captures {
    names: Vec<String>,
    ids: HashMap<String, u32>,
}

fn captures() -> &'static Mutex<Captures> {
    static C: OnceLock<Mutex<Captures>> = OnceLock::new();
    C.get_or_init(Mutex::default)
}

fn capture_id(name: &str) -> u32 {
    let mut c = captures().lock().unwrap();
    if let Some(&id) = c.ids.get(name) {
        return id;
    }
    let id = c.names.len() as u32;
    c.names.push(name.to_string());
    c.ids.insert(name.to_string(), id);
    id
}

/// Table of global capture number → something (usually a theme style). Index with `.get()` using the numbers
/// from `highlights` (another thread may load a new language after the table is built, growing the numbers).
pub fn capture_styles<T>(mut f: impl FnMut(&str) -> T) -> Vec<T> {
    captures().lock().unwrap().names.iter().map(|n| f(n)).collect()
}

#[cfg(test)]
pub fn capture_name(id: u32) -> String {
    captures().lock().unwrap().names[id as usize].clone()
}

// ── Grammar·query loading ───────────────────────────────────────────────

pub struct LangData {
    pub name: String,
    pub language: Language,
    pub highlights: Query,
    /// For `mi`/`ma` syntax objects (None if absent or failed to compile — text-based objects still work).
    pub textobjects: Option<Query>,
    /// Where other languages get injected (markdown code blocks·TODO in comments·macros …) — None if absent.
    pub injections: Option<Query>,
    /// This language's capture number → global capture number (same name = same number across languages —
    /// painted with one style table).
    global: Vec<u32>,
}

/// Cache of loaded languages + open shared libraries (Language points into them, so they're kept alive).
#[derive(Default)]
pub struct Loader {
    langs: Mutex<HashMap<String, Arc<LangData>>>,
    libs: Mutex<Vec<libloading::Library>>,
    /// Languages that failed to load (language → error) — no grammar, ABI mismatch, broken query — so
    /// often-looked-up ones like `comment` (injected into every comment) aren't retried (disk·dlopen) on every
    /// parse. Cleared when a grammar is downloaded (`forget_missing`).
    failed: Mutex<HashMap<String, String>>,
}

impl Loader {
    pub fn global() -> &'static Loader {
        static LOADER: OnceLock<Loader> = OnceLock::new();
        LOADER.get_or_init(Loader::default)
    }

    /// After downloading a new grammar — clears the memory of failed loads.
    pub fn forget_missing(&self) {
        self.failed.lock().unwrap().clear();
    }

    /// Already-loaded language only (never touches disk) — safe on the main thread.
    pub fn loaded(&self, name: &str) -> Option<Arc<LangData>> {
        self.langs.lock().unwrap().get(name).cloned()
    }

    /// Slow (dlopen + query compilation, a few to tens of ms) — call from a worker thread.
    pub fn load(&self, spec: &LangSpec) -> Result<Arc<LangData>, String> {
        if let Some(l) = self.langs.lock().unwrap().get(&spec.name) {
            return Ok(l.clone());
        }
        if let Some(e) = self.failed.lock().unwrap().get(&spec.name) {
            return Err(e.clone());
        }
        let data = self.load_uncached(spec).inspect_err(|e| {
            self.failed.lock().unwrap().insert(spec.name.clone(), e.clone());
        })?;
        self.langs.lock().unwrap().insert(spec.name.clone(), data.clone());
        Ok(data)
    }

    fn load_uncached(&self, spec: &LangSpec) -> Result<Arc<LangData>, String> {
        let language = self.load_grammar(&spec.grammar)?;
        let source = read_query(&spec.name, "highlights.scm", 0)
            .ok_or_else(|| format!("{}: no highlights.scm in runtime", spec.name))?;
        let highlights = Query::new(&language, &source).map_err(|e| format!("{}: query: {e}", spec.name))?;
        let textobjects =
            read_query(&spec.name, "textobjects.scm", 0).and_then(|src| Query::new(&language, &src).ok());
        let injections =
            read_query(&spec.name, "injections.scm", 0).and_then(|src| Query::new(&language, &src).ok());
        let global = highlights.capture_names().iter().map(|n| capture_id(n)).collect();
        Ok(Arc::new(LangData {
            name: spec.name.clone(),
            language,
            highlights,
            textobjects,
            injections,
            global,
        }))
    }

    fn load_grammar(&self, grammar: &str) -> Result<Language, String> {
        // Grammars in the binary first, otherwise a downloaded .so
        if let Some(func) = runtime::builtin_grammar(grammar) {
            // SAFETY: language function of a tree-sitter grammar compiled in by build.rs — takes no args,
            // returns a static language pointer.
            return Ok(Language::new(unsafe { tree_sitter_language::LanguageFn::from_raw(func) }));
        }
        let file = format!("grammars/{grammar}.so");
        let path =
            runtime::find(&file).ok_or_else(|| format!("no grammar '{grammar}' — :grammar-install"))?;
        let symbol = format!("tree_sitter_{}", grammar.replace('-', "_"));
        // SAFETY: language function of a grammar library built by the tree-sitter CLI — takes no args,
        // returns a static language pointer.
        let (lib, language) = unsafe {
            let lib = libloading::Library::new(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let func: libloading::Symbol<unsafe extern "C" fn() -> *const ()> =
                lib.get(symbol.as_bytes()).map_err(|e| format!("{grammar}: {e}"))?;
            let language = Language::new(tree_sitter_language::LanguageFn::from_raw(*func));
            (lib, language)
        };
        let v = language.abi_version();
        if !(tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION).contains(&v) {
            return Err(format!("grammar '{grammar}': ABI {v} not supported"));
        }
        // Kept open only once accepted (`language` points into it)
        self.libs.lock().unwrap().push(lib);
        Ok(language)
    }
}

/// `queries/<lang>/<file>` — replaces `; inherits: a,b` lines with the parent queries' contents.
fn read_query(lang: &str, file: &str, depth: usize) -> Option<String> {
    if depth > 8 {
        return None;
    }
    let src = runtime::query(&format!("{lang}/{file}"))?;
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        match line.trim().strip_prefix("; inherits:") {
            Some(parents) => {
                for p in parents.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    out.push_str(&read_query(p, file, depth + 1).unwrap_or_default());
                    out.push('\n');
                }
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    Some(out)
}

// ── Per-document syntax state ───────────────────────────────────────────

/// One injected-language layer — a tree parsing only part of the text (`set_included_ranges`) as that
/// language.
#[derive(Clone)]
pub struct Layer {
    pub lang: Arc<LangData>,
    pub tree: Tree,
}

/// Tree + injected layers (undo snapshots carry the whole thing — cloning only bumps refcounts).
pub type Trees = (Tree, Vec<Layer>);

pub struct Syntax {
    pub lang: Arc<LangData>,
    pub tree: Option<Tree>,
    /// Injected languages — outer layers first, inner later (later ones paint on top).
    pub layers: Vec<Layer>,
    /// Whether a background parse is running.
    pub in_flight: bool,
    /// Whether there were edits since the last parse started (need to reparse).
    pub dirty: bool,
    /// Edits that arrived mid-parse — reapplied when the new tree arrives.
    pending_edits: Vec<InputEdit>,
    /// Bumped when the text is replaced wholesale (like undo), to drop parse results run on the old text.
    pub generation: u64,
}

impl Syntax {
    pub fn new(lang: Arc<LangData>) -> Self {
        Self {
            lang,
            tree: None,
            layers: Vec::new(),
            in_flight: false,
            dirty: true,
            pending_edits: Vec::new(),
            generation: 0,
        }
    }

    pub fn edit(&mut self, edits: &[InputEdit]) {
        if let Some(tree) = &mut self.tree {
            for e in edits {
                tree.edit(e);
            }
        }
        for l in &mut self.layers {
            for e in edits {
                l.tree.edit(e);
            }
        }
        if self.in_flight {
            self.pending_edits.extend_from_slice(edits);
        }
        self.dirty = true;
    }

    pub fn trees(&self) -> Option<Trees> {
        Some((self.tree.clone()?, self.layers.clone()))
    }

    /// Text replaced wholesale (undo/redo) — use the tree matching that text if there is one.
    pub fn reset(&mut self, trees: Option<Trees>) {
        (self.tree, self.layers) = match trees {
            Some((t, l)) => (Some(t), l),
            None => (None, Vec::new()),
        };
        self.pending_edits.clear();
        self.generation += 1;
        self.dirty = true;
    }

    /// Starts a parse — returns the input to send to the worker thread.
    pub fn start_parse(&mut self, text: &Rope) -> ParseJob {
        self.in_flight = true;
        self.dirty = false;
        self.pending_edits.clear();
        ParseJob {
            text: text.clone(),
            old: self.tree.clone(),
            lang: self.lang.clone(),
            generation: self.generation,
        }
    }

    /// Parse result arrived.
    pub fn finish_parse(&mut self, generation: u64, parsed: Parsed) {
        self.in_flight = false;
        if generation != self.generation {
            self.dirty = true;
            return;
        }
        if let Some(mut tree) = parsed.tree {
            let mut layers = parsed.layers;
            for e in self.pending_edits.drain(..) {
                tree.edit(&e);
                for l in &mut layers {
                    l.tree.edit(&e);
                }
            }
            self.tree = Some(tree);
            self.layers = layers;
        }
    }
}

pub struct ParseJob {
    text: Rope,
    old: Option<Tree>,
    lang: Arc<LangData>,
    pub generation: u64,
}

/// Parse result: tree + injected layers.
pub struct Parsed {
    pub tree: Option<Tree>,
    pub layers: Vec<Layer>,
}

/// Feeds rope chunks directly (no full copy).
fn parse_rope(parser: &mut Parser, text: &Rope, old: Option<&Tree>) -> Option<Tree> {
    let len = text.len_bytes();
    let mut read = |byte: usize, _: Point| -> &[u8] {
        if byte >= len {
            return &[];
        }
        let (chunk, start, _, _) = text.chunk_at_byte(byte);
        &chunk.as_bytes()[byte - start..]
    };
    parser.parse_with_options(&mut read, old, None)
}

impl ParseJob {
    /// On a worker thread — incremental parse after edits. Injected languages are parsed fresh each time
    /// (usually small pieces).
    pub fn run(self) -> Parsed {
        let mut parser = Parser::new();
        let tree = parser
            .set_language(&self.lang.language)
            .ok()
            .and_then(|_| parse_rope(&mut parser, &self.text, self.old.as_ref()));
        let mut layers = Vec::new();
        if let Some(tree) = &tree {
            let mut inj =
                Injector { text: &self.text, parsers: HashMap::new(), spent: std::time::Duration::ZERO };
            inj.run(&self.lang, tree, 0, &mut layers);
        }
        Parsed { tree, layers }
    }
}

// ── Injections ──────────────────────────────────────────────────────────

/// Caps on layer count·depth·time per document — beyond them the rest keeps the outer colors (not wrong).
const MAX_LAYERS: usize = 2000;
const MAX_DEPTH: usize = 3;
const INJECT_BUDGET: std::time::Duration = std::time::Duration::from_millis(80);

struct Injector<'a> {
    text: &'a Rope,
    parsers: HashMap<String, Parser>,
    /// Time spent parsing injected languages (grammar loading·query compilation happen once, so not counted).
    spent: std::time::Duration,
}

/// Node range — children excluded (default), only unnamed children included (`include-unnamed-children`),
/// or whole (`include-children`).
fn node_ranges(node: Node, children: bool, unnamed: bool) -> Vec<tree_sitter::Range> {
    if children {
        return vec![node.range()];
    }
    let mut out = Vec::new();
    let (mut sb, mut sp) = (node.start_byte(), node.start_position());
    let mut cur = node.walk();
    for child in node.children(&mut cur) {
        if unnamed && !child.is_named() {
            continue;
        }
        if child.start_byte() > sb {
            out.push(tree_sitter::Range {
                start_byte: sb,
                end_byte: child.start_byte(),
                start_point: sp,
                end_point: child.start_position(),
            });
        }
        (sb, sp) = (child.end_byte().max(sb), child.end_position());
    }
    if node.end_byte() > sb {
        out.push(tree_sitter::Range {
            start_byte: sb,
            end_byte: node.end_byte(),
            start_point: sp,
            end_point: node.end_position(),
        });
    }
    out
}

/// A node's text — ends snapped to char boundaries, so a briefly stale tree (edits not yet reparsed) stays
/// safe to slice.
fn node_slice<'a>(text: &'a Rope, node: Node) -> ropey::RopeSlice<'a> {
    let len = text.len_bytes();
    let snap = |b: usize| text.char_to_byte(text.byte_to_char(b.min(len)));
    let (s, e) = (snap(node.start_byte()), snap(node.end_byte()));
    text.byte_slice(s..e.max(s))
}

/// `#!/usr/bin/env python3` → `python3`.
fn shebang(first_line: &str) -> Option<String> {
    let rest = first_line.strip_prefix("#!")?;
    let mut words = rest.split_whitespace();
    let prog = words.next()?.rsplit('/').next()?;
    let prog = if prog == "env" { words.find(|w| !w.starts_with('-'))? } else { prog };
    Some(prog.to_string())
}

impl Injector<'_> {
    fn node_text(&self, node: Node) -> String {
        node_slice(self.text, node).chars().take(200).collect()
    }

    fn run(&mut self, host: &Arc<LangData>, tree: &Tree, depth: usize, out: &mut Vec<Layer>) {
        let Some(q) = &host.injections else { return };
        if depth >= MAX_DEPTH {
            return;
        }
        let idx = |name: &str| q.capture_names().iter().position(|n| *n == name).map(|i| i as u32);
        let (content_i, lang_i, shebang_i, file_i) = (
            idx("injection.content"),
            idx("injection.language"),
            idx("injection.shebang"),
            idx("injection.filename"),
        );
        let text = self.text;
        let provider = |node: Node| node_slice(text, node).chunks().map(str::as_bytes);
        // (language, ranges) — combined ones (`injection.combined`) become one per (pattern, language)
        let mut single: Vec<(String, Vec<tree_sitter::Range>)> = Vec::new();
        let mut combined: Vec<((usize, String), Vec<tree_sitter::Range>)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(q, tree.root_node(), provider);
        while let Some(m) = matches.next() {
            let props = q.property_settings(m.pattern_index);
            let prop = |k: &str| props.iter().find(|p| &*p.key == k);
            let mut lang = prop("injection.language").and_then(|p| p.value.as_deref().map(str::to_string));
            let mut content = Vec::new();
            for c in m.captures() {
                let i = Some(c.index);
                if i == content_i {
                    content.push(c.node);
                }
                if i == lang_i {
                    lang = Some(self.node_text(c.node));
                } else if i == shebang_i && lang.is_none() {
                    let first = self.node_text(c.node);
                    lang = shebang(first.lines().next().unwrap_or_default());
                } else if i == file_i && lang.is_none() {
                    let f = self.node_text(c.node);
                    lang = detect(Path::new(f.trim())).map(|s| s.name.clone());
                }
            }
            let Some(lang) = lang else { continue };
            let (children, unnamed) = (
                prop("injection.include-children").is_some(),
                prop("injection.include-unnamed-children").is_some(),
            );
            let mut ranges = Vec::new();
            for node in content {
                // When two patterns catch the same spot (shebang·language name), only the first one
                if seen.insert((node.start_byte(), node.end_byte())) {
                    ranges.extend(node_ranges(node, children, unnamed));
                }
            }
            if ranges.is_empty() {
                continue;
            }
            if prop("injection.combined").is_some() {
                let key = (m.pattern_index, lang);
                match combined.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, r)) => r.extend(ranges),
                    None => combined.push((key, ranges)),
                }
            } else {
                single.push((lang, ranges));
            }
        }
        drop(matches);
        let all = combined.into_iter().map(|((_, l), r)| (l, r)).chain(single);
        for (lang, mut ranges) in all {
            if out.len() >= MAX_LAYERS || self.spent > INJECT_BUDGET {
                return;
            }
            let Some(data) = resolve(&lang).and_then(|s| Loader::global().load(s).ok()) else { continue };
            ranges.sort_by_key(|r| r.start_byte);
            ranges.dedup_by(|b, a| b.start_byte < a.end_byte);
            let parser = self.parsers.entry(data.name.clone()).or_insert_with(|| {
                let mut p = Parser::new();
                let _ = p.set_language(&data.language);
                p
            });
            if parser.set_included_ranges(&ranges).is_err() {
                continue;
            }
            let t = std::time::Instant::now();
            let tree = parse_rope(parser, self.text, None);
            self.spent += t.elapsed();
            let Some(tree) = tree else { continue };
            out.push(Layer { lang: data.clone(), tree: tree.clone() });
            self.run(&data, &tree, depth + 1, out);
        }
    }
}

// ── Definition location (header path) ─────────────────────────────────

/// Definitions enclosing the cursor, outer → inner: (kind 'f' function · 't' type · 'm' module, name).
/// Guessed from node names, not per-grammar queries (`function_item`, `class_definition`, `impl_item` …).
pub fn scope_path(syn: &Syntax, text: &Rope, pos: usize) -> Vec<(char, String)> {
    let Some(tree) = &syn.tree else { return Vec::new() };
    let len = text.len_bytes();
    let mut node = tree.root_node().descendant_for_byte_range(pos.min(len), pos.min(len));
    let mut out = Vec::new();
    while let Some(n) = node {
        let k = n.kind();
        let def = ["_item", "_definition", "_declaration", "_specifier"].iter().any(|s| k.ends_with(s));
        let class = if !def {
            None
        } else if k.contains("function") || k.contains("method") {
            Some('f')
        } else if k == "impl_item"
            || ["struct", "enum", "trait", "class", "interface", "type_item", "union"]
                .iter()
                .any(|w| k.starts_with(w))
        {
            Some('t')
        } else if k.starts_with("mod") || k.starts_with("namespace") {
            Some('m')
        } else {
            None
        };
        if let Some(c) = class
            && let Some(name) = n.child_by_field_name("name").or_else(|| n.child_by_field_name("type"))
        {
            let s: String = node_slice(text, name).chars().take(40).collect();
            if !s.is_empty() {
                out.push((c, s));
            }
        }
        node = n.parent();
    }
    out.reverse();
    out
}

// ── Edit → InputEdit ────────────────────────────────────────────────────

fn point(text: &Rope, byte: usize) -> Point {
    let row = text.byte_to_line(byte);
    Point { row, column: byte - text.line_to_byte(row) }
}

/// Call **right before** applying one change to the rope (positions relative to the text at that moment).
pub fn input_edit(text: &Rope, from: usize, to: usize, insert: &str) -> InputEdit {
    let start = point(text, from);
    let new_end = match insert.rfind('\n') {
        Some(i) => Point { row: start.row + insert.matches('\n').count(), column: insert.len() - i - 1 },
        None => Point { row: start.row, column: start.column + insert.len() },
    };
    InputEdit {
        start_byte: from,
        old_end_byte: to,
        new_end_byte: from + insert.len(),
        start_position: start,
        old_end_position: point(text, to),
        new_end_position: new_end,
    }
}

// ── Highlighting ────────────────────────────────────────────────────────

/// Captures of one language·one tree (start, end, global capture number, pattern number).
fn query_spans(
    lang: &LangData,
    tree: &Tree,
    text: &Rope,
    range: &std::ops::Range<usize>,
    cursor: &mut QueryCursor,
) -> Vec<(usize, usize, u32, usize)> {
    cursor.set_byte_range(range.clone());
    // For text predicates (#eq? #match? …)
    let provider = |node: Node| node_slice(text, node).chunks().map(str::as_bytes);
    let mut spans = Vec::new();
    let mut caps = cursor.captures(&lang.highlights, tree.root_node(), provider);
    while let Some((m, i)) = caps.next() {
        let c = m.captures()[*i];
        let (s, e) = (c.node.start_byte().max(range.start), c.node.end_byte().min(range.end));
        if s < e {
            spans.push((s, e, lang.global[c.index as usize], m.pattern_index));
        }
    }
    spans.sort_by(|a, b| (b.1 - b.0).cmp(&(a.1 - a.0)).then(a.3.cmp(&b.3)));
    spans
}

/// (start, end, global capture number — index into the `capture_styles` table) within `range`. In draw
/// order — outer first, same node earlier pattern first (so inner·later patterns paint on top and win).
/// Injected languages come after (painted on top).
pub fn highlights(
    syn: &Syntax,
    text: &Rope,
    range: std::ops::Range<usize>,
    cursor: &mut QueryCursor,
) -> Vec<(usize, usize, u32)> {
    let Some(tree) = &syn.tree else { return Vec::new() };
    let strip = |v: Vec<(usize, usize, u32, usize)>| v.into_iter().map(|(s, e, c, _)| (s, e, c));
    let mut out: Vec<(usize, usize, u32)> =
        strip(query_spans(&syn.lang, tree, text, &range, cursor)).collect();
    for l in &syn.layers {
        let r = l.tree.root_node().byte_range();
        if r.end <= range.start || r.start >= range.end {
            continue;
        }
        let spans = query_spans(&l.lang, &l.tree, text, &range, cursor);
        // Layers stitched from several pieces (Helm's YAML etc.) have nodes spanning the gaps (template
        // spots) — paint only within the given ranges
        let pieces = l.tree.included_ranges();
        if pieces.len() <= 1 {
            out.extend(strip(spans));
            continue;
        }
        for (s, e, c, _) in spans {
            let from = pieces.partition_point(|p| p.end_byte <= s);
            for p in pieces[from..].iter().take_while(|p| p.start_byte < e) {
                let (a, b) = (s.max(p.start_byte), e.min(p.end_byte));
                if a < b {
                    out.push((a, b, c));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_languages() {
        let name = |p: &str| detect(Path::new(p)).map(|s| s.name.as_str());
        assert_eq!(name("src/main.rs"), Some("rust"));
        assert_eq!(name("/x/Makefile"), Some("make"));
        assert_eq!(name("/home/u/.ssh/config"), Some("sshclientconfig"));
        assert_eq!(name("/proj/config"), None, "common names only by path");
        assert_eq!(name("a.tsx"), Some("tsx"));
        assert_eq!(spec("protobuf").map(|s| s.grammar.as_str()), Some("proto"));
        // Helm chart templates are helm, not YAML (Go template + text between injected as YAML)
        assert_eq!(name("/charts/app/templates/deployment.yaml"), Some("helm"));
        assert_eq!(name("/charts/app/templates/_helpers.tpl"), Some("helm"));
        assert_eq!(name("/charts/app/templates/NOTES.txt"), Some("helm"));
        assert_eq!(name("/charts/app/values.yaml"), Some("yaml"));
        assert_eq!(name("/x/tmpl.gotmpl"), Some("gotmpl"));
    }

    #[test]
    fn input_edit_points() {
        let t = Rope::from_str("ab\ncd");
        let e = input_edit(&t, 1, 4, "X\nYZ");
        assert_eq!(e.start_position, Point { row: 0, column: 1 });
        assert_eq!(e.old_end_position, Point { row: 1, column: 1 });
        assert_eq!(e.new_end_position, Point { row: 1, column: 2 });
        assert_eq!(e.new_end_byte, 5);
    }

    /// Only when the real grammar is present (downloaded or built in) — skipped otherwise.
    #[test]
    fn highlights_rust_when_grammar_available() {
        let Ok(lang) = Loader::global().load(spec("rust").unwrap()) else { return };
        let text = Rope::from_str("fn main() { let x = \"hi\"; }\n");
        let mut syn = Syntax::new(lang.clone());
        let job = syn.start_parse(&text);
        let generation = job.generation;
        syn.finish_parse(generation, job.run());
        let spans = highlights(&syn, &text, 0..text.len_bytes(), &mut QueryCursor::new());
        let names: Vec<String> = spans.iter().map(|&(_, _, c)| capture_name(c)).collect();
        assert!(names.iter().any(|n| n.starts_with("keyword")), "{names:?}");
        assert!(names.iter().any(|n| n == "string"), "{names:?}");
        // The last capture painted on "fn" (0..2) is keyword-ish
        let fn_kw = spans.iter().rev().find(|&&(s, e, _)| s == 0 && e == 2).unwrap();
        assert!(capture_name(fn_kw.2).starts_with("keyword"));
    }

    #[test]
    fn resolves_fence_names_and_shebangs() {
        let name = |n: &str| resolve(n).map(|s| s.name.as_str());
        assert_eq!(name("rust"), Some("rust"));
        assert_eq!(name("rs"), Some("rust"));
        assert_eq!(name("sh"), Some("bash"));
        assert_eq!(name("Python3"), Some("python"));
        assert_eq!(name("text"), None);
        assert_eq!(shebang("#!/usr/bin/env python3").as_deref(), Some("python3"));
        assert_eq!(shebang("#!/bin/bash -e").as_deref(), Some("bash"));
        assert_eq!(shebang("print(1)"), None);
    }

    /// The inside of a markdown code block is colored as that language (only on machines with the grammar).
    #[test]
    fn injects_fenced_code_when_grammars_available() {
        let (Ok(md), Ok(_)) =
            (Loader::global().load(spec("markdown").unwrap()), Loader::global().load(spec("rust").unwrap()))
        else {
            return;
        };
        let src = "# Title\n\nSome **bold** text.\n\n```rust\nfn main() {}\n```\n";
        let text = Rope::from_str(src);
        let mut syn = Syntax::new(md);
        let job = syn.start_parse(&text);
        let generation = job.generation;
        syn.finish_parse(generation, job.run());
        assert!(syn.layers.iter().any(|l| l.lang.name == "rust"), "code block layer");
        let fn_at = src.find("fn main").unwrap();
        let spans = highlights(&syn, &text, 0..text.len_bytes(), &mut QueryCursor::new());
        let on_fn = spans.iter().rev().find(|&&(s, e, _)| s <= fn_at && fn_at < e).unwrap();
        assert!(capture_name(on_fn.2).starts_with("keyword"), "{}", capture_name(on_fn.2));
        // Edits shift the layer: inserting a line at the very top makes fn paint that much later too
        let tx = crate::transaction::Transaction::new(vec![crate::transaction::Change::insert(0, "x\n")]);
        let mut t2 = text.clone();
        let edits = tx.apply(&mut t2, false);
        syn.edit(&edits.iter().map(|e| e.ts).collect::<Vec<_>>());
        let spans = highlights(&syn, &t2, 0..t2.len_bytes(), &mut QueryCursor::new());
        let on_fn = spans.iter().rev().find(|&&(s, e, _)| s <= fn_at + 2 && fn_at + 2 < e).unwrap();
        assert!(capture_name(on_fn.2).starts_with("keyword"));
        if Loader::global().load(spec("markdown.inline").unwrap()).is_ok() {
            let b = src.find("bold").unwrap();
            let on_b = spans.iter().rev().find(|&&(s, e, _)| s <= b + 2 && b + 2 < e).unwrap();
            assert!(capture_name(on_b.2).contains("bold"), "{}", capture_name(on_b.2));
        }
    }

    /// Built-in queries mesh with every downloaded grammar (catches queries broken by a grammar bump).
    /// Missing grammars are skipped.
    #[test]
    fn every_installed_grammar_loads_with_its_queries() {
        let mut checked = 0;
        for spec in specs() {
            let installed = crate::runtime::builtin_grammar(&spec.grammar).is_some()
                || crate::runtime::find(format!("grammars/{}.so", spec.grammar)).is_some();
            if !installed {
                continue;
            }
            if let Err(e) = Loader::global().load(spec) {
                panic!("{}: {e}", spec.name);
            }
            checked += 1;
        }
        eprintln!("{checked} languages checked");
    }

    /// Helm templates: `{{ }}` in Go template colors, text between is a YAML layer (not HTML) — templates
    /// inside quotes aren't covered by the YAML string.
    #[test]
    fn helm_templates_color_yaml_between_actions() {
        // Skip if a build without bundled grammars hasn't downloaded it yet
        let (Ok(lang), Ok(_)) =
            (Loader::global().load(spec("helm").unwrap()), Loader::global().load(spec("yaml").unwrap()))
        else {
            return;
        };
        let src = "kind: Deployment\nimage: \"{{ .Values.tag }}\"\n";
        let text = Rope::from_str(src);
        let mut syn = Syntax::new(lang);
        let job = syn.start_parse(&text);
        let g = job.generation;
        syn.finish_parse(g, job.run());
        assert_eq!(syn.layers.iter().map(|l| l.lang.name.as_str()).collect::<Vec<_>>(), ["yaml"]);
        let spans = highlights(&syn, &text, 0..text.len_bytes(), &mut QueryCursor::new());
        let top = |at: usize| {
            spans.iter().rev().find(|&&(s, e, _)| s <= at && at < e).map(|&(_, _, c)| capture_name(c))
        };
        assert!(top(src.find("Deployment").unwrap()).is_some_and(|c| c.starts_with("string")), "YAML value");
        let values = src.find("Values").unwrap();
        assert!(!top(values).unwrap_or_default().starts_with("string"), "in quotes: template color");
    }
}
