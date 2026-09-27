//! Signature help — typing `(`·`,` shows the signature above the cursor, highlighting the current argument.
//! Response parsing (pure part) lives here, requests/re-requests in `lsp_editor.rs`, drawing in `term.rs`.

use serde_json::Value;

use crate::document::DocId;

#[derive(Debug, PartialEq)]
pub struct Signature {
    pub doc: DocId,
    /// Full signature (`fn get(&self, k: &Q) -> Option<&V>`).
    pub label: String,
    /// Byte range of the current argument within label.
    pub active: Option<(usize, usize)>,
    /// Which of several signatures (overloads) / total.
    pub index: usize,
    pub count: usize,
    /// Current argument's docs (else the first paragraph of the signature docs) — raw markdown.
    pub docs: String,
}

impl Signature {
    /// `SignatureHelp` response → the one signature to show. None if there is none (= close).
    pub fn from_lsp(doc: DocId, v: &Value) -> Option<Signature> {
        let sigs = v["signatures"].as_array().filter(|a| !a.is_empty())?;
        let index = v["activeSignature"].as_u64().map_or(0, |i| i as usize).min(sigs.len() - 1);
        let sig = &sigs[index];
        let label = sig["label"].as_str()?.to_string();
        // Per-signature activeParameter (3.16+) wins if present
        let active_param = sig["activeParameter"].as_u64().or_else(|| v["activeParameter"].as_u64());
        let param = active_param.and_then(|i| sig["parameters"].as_array()?.get(i as usize));
        let active = param.and_then(|p| param_range(&label, &p["label"]));
        let docs = param
            .map(|p| markup(&p["documentation"]))
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| {
                let d = markup(&sig["documentation"]);
                d.split("\n\n").next().unwrap_or_default().trim().to_string()
            });
        Some(Signature { doc, label, active, index, count: sigs.len(), docs })
    }
}

/// Argument label: a string is searched in the signature (word boundary); `[start, end]` = UTF-16 offsets.
fn param_range(label: &str, p: &Value) -> Option<(usize, usize)> {
    match p {
        Value::String(s) if !s.is_empty() => {
            // Don't match "a" inside "add" of "fn add(a: i32)": search after the opening paren
            let from = label.find('(').map_or(0, |i| i + 1);
            label[from..].find(s.as_str()).map(|i| (from + i, from + i + s.len()))
        }
        Value::Array(a) if a.len() == 2 => {
            let (s, e) = (a[0].as_u64()? as usize, a[1].as_u64()? as usize);
            Some((utf16_to_byte(label, s)?, utf16_to_byte(label, e)?))
        }
        _ => None,
    }
}

fn utf16_to_byte(s: &str, units: usize) -> Option<usize> {
    let mut n = 0;
    for (b, c) in s.char_indices() {
        if n >= units {
            return Some(b);
        }
        n += c.len_utf16();
    }
    (n >= units).then_some(s.len())
}

fn markup(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => o.get("value").and_then(Value::as_str).unwrap_or_default().to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn offsets_strings_and_overloads() {
        let v = json!({ "signatures": [
            { "label": "fn add(a: i32, 타래: &str)", "parameters": [{ "label": [7, 13] }, { "label": [15, 23] }],
              "documentation": "Adds.\n\nMore." },
        ], "activeSignature": 0, "activeParameter": 1 });
        let s = Signature::from_lsp(1, &v).unwrap();
        assert_eq!(&s.label[s.active.unwrap().0..s.active.unwrap().1], "타래: &str", "UTF-16 offset → byte");
        assert_eq!(s.docs, "Adds.", "no argument docs → first paragraph of the signature docs");
        let v = json!({ "signatures": [
            { "label": "f()" },
            { "label": "f(a)", "parameters": [{ "label": "a", "documentation": { "kind": "markdown", "value": "the `a`" } }], "activeParameter": 0 },
        ], "activeSignature": 1 });
        let s = Signature::from_lsp(1, &v).unwrap();
        assert_eq!((s.index, s.count, s.active, s.docs.as_str()), (1, 2, Some((2, 3)), "the `a`"));
        assert_eq!(Signature::from_lsp(1, &json!({ "signatures": [] })), None);
        assert_eq!(Signature::from_lsp(1, &json!(null)), None);
    }
}
