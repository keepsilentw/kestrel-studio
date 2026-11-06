//! A host-side guard over the DOM modules' own source.
//!
//! `HTMLElement::dataset()` hands back a `DomStringMap`, whose keys are the
//! **camelCase** property names: the attribute `data-conversation-id` is read as
//! `dataset["conversationId"]`. Asking it for `"conversation-id"` neither errors
//! nor panics — it is a silent `None`, so every reader takes its `None` branch and
//! the page keeps working badly. This is the failure the WASM migration signed up
//! for (docs/roadmap.md §4.1: the client has no runtime guard beyond `cargo test`),
//! and it was invisible to both the host tests (they assert the pure decisions, not
//! the DOM) and the template contract test (which pins element *ids*, not attribute
//! *keys*). Six such lookups shipped; five of them were load-bearing. See
//! docs/verification.md §22.
//!
//! So the spelling is asserted against the source that ships, the same way
//! `rust/server/src/view/tests.rs` reads the ids out of the client it serves.

/// Every `dataset().get("…")` literal in `source`, with its byte offset.
fn dataset_literals(source: &str) -> Vec<(usize, String)> {
    const CALL: &str = "dataset().get(\"";
    let mut found = Vec::new();
    let mut offset = 0;
    while let Some(at) = source[offset..].find(CALL) {
        let start = offset + at + CALL.len();
        let Some(end) = source[start..].find('"') else {
            break;
        };
        found.push((start, source[start..start + end].to_string()));
        offset = start + end;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::dataset_literals;

    /// The modules that can touch the DOM. `chat_core` / `voice_core` / `pcm`
    /// cannot, so scanning them would only add files to keep in sync.
    const DOM_MODULES: [&str; 5] = ["chat_page.rs", "voice_page.rs", "wasm.rs", "audio.rs", "wire.rs"];

    #[test]
    fn dataset_keys_are_camel_case() {
        let mut checked = 0;
        for name in DOM_MODULES {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join(name);
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{} must be readable for the contract test: {error}", path.display()));
            for (byte, key) in dataset_literals(&source) {
                checked += 1;
                assert!(
                    !key.contains('-') && !key.contains('_'),
                    "{}: `dataset().get(\"{key}\")` at byte {byte} uses the attribute spelling — \
                     `data-{key}` is not a property of the map; read it in camelCase",
                    path.display()
                );
            }
        }
        assert!(
            checked >= 8,
            "only found {checked} dataset lookups across {DOM_MODULES:?} — the parser is out of date"
        );
    }

    #[test]
    fn the_parser_sees_both_spellings() {
        let source = r#"let a = body.dataset().get("conversation-id");
                         let b = body.dataset().get("conversationId");"#;
        let keys: Vec<String> = dataset_literals(source).into_iter().map(|(_, key)| key).collect();
        assert_eq!(keys, ["conversation-id", "conversationId"]);
    }
}
