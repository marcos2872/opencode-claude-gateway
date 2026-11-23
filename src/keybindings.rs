//! Claude Code keybindings: the chord that hides and shows the band above the
//! prompt (`agent-usage`'s row there).
//!
//! A plugin **cannot** register a chord itself: `plugin.json` has no
//! `keybindings` field (the validator calls it an unknown field and ignores
//! it), and the mods API only lets a `Button` name an action the engine
//! already has. So the shortcut is a *user* binding, written into
//! `~/.claude/keybindings.json` — which is what this module does, merging into
//! whatever is already there instead of overwriting it.
//!
//! The binding is additive: `ctrl+x ctrl+a` keeps working, since a key set to
//! an action adds a chord to that action. `--force` unbinds the old chord.

use std::path::{Path, PathBuf};

/// The engine action that collapses/expands the band above the prompt. It is
/// the whole band, not one plugin's row: the engine has no per-plugin action.
pub const BAND_TOGGLE_ACTION: &str = "abovePrompt:toggle";
/// The chord `ocg install --agent-usage` adds.
pub const BAND_TOGGLE_KEY: &str = "ctrl+x o";
/// The engine's own chord for the same action, kept unless `--force`.
pub const BAND_TOGGLE_DEFAULT_KEY: &str = "ctrl+x ctrl+a";
/// The context the chords live in (`abovePrompt:toggle` defaults to it).
pub const BAND_TOGGLE_CONTEXT: &str = "Chat";

pub const SCHEMA_URL: &str = "https://www.schemastore.org/claude-code-keybindings.json";
pub const DOCS_URL: &str = "https://code.claude.com/docs/en/keybindings";

/// `~/.claude/keybindings.json`.
pub fn keybindings_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("keybindings.json")
}

/// What `install_keybinding` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingWrite {
    /// A new file with just this binding.
    Created { path: PathBuf },
    /// The chord was added to an existing file (the old chord kept).
    Added { path: PathBuf },
    /// The chord was added and the engine's old chord unbound.
    Moved { path: PathBuf },
    /// The chord was already bound to the action.
    Already { path: PathBuf },
    /// The chord is bound to a different action; left alone.
    Conflict { path: PathBuf, action: String },
}

impl BindingWrite {
    pub fn path(&self) -> &Path {
        match self {
            BindingWrite::Created { path }
            | BindingWrite::Added { path }
            | BindingWrite::Moved { path }
            | BindingWrite::Already { path }
            | BindingWrite::Conflict { path, .. } => path,
        }
    }
}

/// Add `key` → `abovePrompt:toggle` under `context`, merging into `existing`
/// (the file's current contents, or `None` when there is no file).
///
/// `move_old` also unbinds `ctrl+x ctrl+a`, so the shortcut is only the new
/// one. Everything else in the file — other contexts, other keys, `$schema` —
/// is preserved as it was.
pub fn merge_keybinding(
    existing: Option<&str>,
    key: &str,
    context: &str,
    move_old: bool,
) -> Result<(serde_json::Value, BindingWriteKind), String> {
    let mut doc: serde_json::Value = match existing {
        Some(text) if !text.trim().is_empty() => serde_json::from_str(text)
            .map_err(|e| format!("cannot parse the existing keybindings.json: {e}"))?,
        _ => serde_json::json!({}),
    };
    if !doc.is_object() {
        return Err("keybindings.json is not a JSON object".to_string());
    }
    let root = doc.as_object_mut().expect("checked above");
    root.entry("$schema")
        .or_insert_with(|| serde_json::Value::String(SCHEMA_URL.to_string()));
    root.entry("$docs")
        .or_insert_with(|| serde_json::Value::String(DOCS_URL.to_string()));
    let bindings = root
        .entry("bindings")
        .or_insert_with(|| serde_json::Value::Array(vec![]));
    let list = bindings
        .as_array_mut()
        .ok_or("keybindings.json: `bindings` is not an array")?;

    // The block for this context, created when the file has none.
    let index = list.iter().position(|entry| {
        entry.get("context").and_then(serde_json::Value::as_str) == Some(context)
    });
    let block = match index {
        Some(i) => &mut list[i],
        None => {
            list.push(serde_json::json!({ "context": context, "bindings": {} }));
            list.last_mut().expect("just pushed")
        }
    };
    if !block.is_object() {
        return Err(format!(
            "keybindings.json: the `{context}` entry is not an object"
        ));
    }
    let keys = block
        .as_object_mut()
        .expect("checked above")
        .entry("bindings")
        .or_insert_with(|| serde_json::json!({}));
    let map = keys
        .as_object_mut()
        .ok_or_else(|| format!("keybindings.json: `{context}` bindings is not an object"))?;

    // Someone else's chord: never steal it.
    if let Some(current) = map.get(key).and_then(serde_json::Value::as_str) {
        let conflict = current != BAND_TOGGLE_ACTION;
        let action = current.to_string();
        return Ok((
            doc,
            if conflict {
                BindingWriteKind::Conflict { action }
            } else {
                BindingWriteKind::Already
            },
        ));
    }

    map.insert(
        key.to_string(),
        serde_json::Value::String(BAND_TOGGLE_ACTION.to_string()),
    );
    let moved = if move_old && map.contains_key(BAND_TOGGLE_DEFAULT_KEY) {
        map.insert(BAND_TOGGLE_DEFAULT_KEY.to_string(), serde_json::Value::Null);
        true
    } else {
        false
    };
    Ok((
        doc,
        if moved {
            BindingWriteKind::Moved
        } else {
            BindingWriteKind::Added
        },
    ))
}

/// The outcome of `merge_keybinding`, before it knows the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingWriteKind {
    Added,
    Moved,
    Already,
    Conflict { action: String },
}

/// Merge the binding into `path`, creating the file when it does not exist.
pub fn install_keybinding(
    path: &Path,
    key: &str,
    context: &str,
    move_old: bool,
) -> Result<BindingWrite, String> {
    let existing = std::fs::read_to_string(path).ok();
    let created = existing.is_none();
    let (doc, kind) = merge_keybinding(existing.as_deref(), key, context, move_old)?;
    let write = match kind {
        BindingWriteKind::Already => {
            return Ok(BindingWrite::Already {
                path: path.to_path_buf(),
            })
        }
        BindingWriteKind::Conflict { action } => {
            return Ok(BindingWrite::Conflict {
                path: path.to_path_buf(),
                action,
            })
        }
        BindingWriteKind::Moved => BindingWrite::Moved {
            path: path.to_path_buf(),
        },
        BindingWriteKind::Added if created => BindingWrite::Created {
            path: path.to_path_buf(),
        },
        BindingWriteKind::Added => BindingWrite::Added {
            path: path.to_path_buf(),
        },
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let mut text = serde_json::to_string_pretty(&doc)
        .map_err(|e| format!("cannot render keybindings.json: {e}"))?;
    text.push('\n');
    write_private(path, text.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(write)
}

fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merged(
        existing: Option<&str>,
        key: &str,
        move_old: bool,
    ) -> (serde_json::Value, BindingWriteKind) {
        merge_keybinding(existing, key, BAND_TOGGLE_CONTEXT, move_old).unwrap()
    }

    #[test]
    fn creates_the_file_shape_when_there_is_none() {
        let (doc, kind) = merged(None, BAND_TOGGLE_KEY, false);
        assert_eq!(kind, BindingWriteKind::Added);
        assert_eq!(doc["$schema"], SCHEMA_URL);
        assert_eq!(doc["bindings"][0]["context"], "Chat");
        assert_eq!(
            doc["bindings"][0]["bindings"]["ctrl+x o"],
            BAND_TOGGLE_ACTION
        );
    }

    #[test]
    fn keeps_other_contexts_and_keys() {
        let existing = r#"{
          "bindings": [
            { "context": "Global", "bindings": { "ctrl+t": "app:toggleTodos" } },
            { "context": "Chat", "bindings": { "ctrl+g": "chat:externalEditor" } }
          ]
        }"#;
        let (doc, kind) = merged(Some(existing), BAND_TOGGLE_KEY, false);
        assert_eq!(kind, BindingWriteKind::Added);
        let list = doc["bindings"].as_array().unwrap();
        assert_eq!(list.len(), 2, "no new context block: Chat already existed");
        assert_eq!(list[0]["bindings"]["ctrl+t"], "app:toggleTodos");
        assert_eq!(list[1]["bindings"]["ctrl+g"], "chat:externalEditor");
        assert_eq!(list[1]["bindings"]["ctrl+x o"], BAND_TOGGLE_ACTION);
    }

    #[test]
    fn appends_a_context_block_when_the_file_has_none() {
        let existing = r#"{ "bindings": [ { "context": "Global", "bindings": { "ctrl+t": "app:toggleTodos" } } ] }"#;
        let (doc, _) = merged(Some(existing), BAND_TOGGLE_KEY, false);
        let list = doc["bindings"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1]["context"], "Chat");
    }

    #[test]
    fn is_idempotent() {
        let (first, _) = merged(None, BAND_TOGGLE_KEY, false);
        let text = serde_json::to_string(&first).unwrap();
        let (_, kind) = merged(Some(&text), BAND_TOGGLE_KEY, false);
        assert_eq!(kind, BindingWriteKind::Already);
    }

    #[test]
    fn never_steals_a_chord_bound_to_another_action() {
        let existing = r#"{ "bindings": [ { "context": "Chat", "bindings": { "ctrl+x o": "chat:stash" } } ] }"#;
        let (_, kind) = merged(Some(existing), BAND_TOGGLE_KEY, false);
        assert_eq!(
            kind,
            BindingWriteKind::Conflict {
                action: "chat:stash".to_string()
            }
        );
    }

    #[test]
    fn move_old_unbinds_the_engine_chord() {
        let existing = r#"{ "bindings": [ { "context": "Chat", "bindings": { "ctrl+x ctrl+a": "abovePrompt:toggle" } } ] }"#;
        let (doc, kind) = merged(Some(existing), BAND_TOGGLE_KEY, true);
        assert_eq!(kind, BindingWriteKind::Moved);
        let map = &doc["bindings"][0]["bindings"];
        assert!(map["ctrl+x ctrl+a"].is_null());
        assert_eq!(map["ctrl+x o"], BAND_TOGGLE_ACTION);
    }

    #[test]
    fn keeps_the_old_chord_by_default() {
        let existing = r#"{ "bindings": [ { "context": "Chat", "bindings": { "ctrl+x ctrl+a": "abovePrompt:toggle" } } ] }"#;
        let (doc, kind) = merged(Some(existing), BAND_TOGGLE_KEY, false);
        assert_eq!(kind, BindingWriteKind::Added);
        let map = &doc["bindings"][0]["bindings"];
        assert_eq!(map["ctrl+x ctrl+a"], BAND_TOGGLE_ACTION);
    }

    #[test]
    fn a_broken_file_is_an_error_not_a_silent_reset() {
        let err = merge_keybinding(Some("{ not json"), BAND_TOGGLE_KEY, "Chat", false).unwrap_err();
        assert!(err.contains("cannot parse"), "{err}");
    }

    #[test]
    fn rounds_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("ocg-kb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keybindings.json");

        let first = install_keybinding(&path, BAND_TOGGLE_KEY, BAND_TOGGLE_CONTEXT, false).unwrap();
        assert!(matches!(first, BindingWrite::Created { .. }));
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            parsed["bindings"][0]["bindings"]["ctrl+x o"],
            BAND_TOGGLE_ACTION
        );

        let again = install_keybinding(&path, BAND_TOGGLE_KEY, BAND_TOGGLE_CONTEXT, false).unwrap();
        assert!(matches!(again, BindingWrite::Already { .. }));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
