//! Stable `pubmed-v1` text assembly, chunking, and point identity.

use serde_json::Value;
use serde_json::json;
use uuid::Uuid;

use super::literature_registry::Literature;

pub(super) const PUBMED_EMBEDDING_PROFILE: &str = "pubmed-v1";
const MAX_CHARS: usize = 2_000;
const OVERLAP_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PubmedChunk {
    pub(super) point_id: String,
    pub(super) chunk_id: String,
    pub(super) chunk_index: usize,
    pub(super) text: String,
}

pub(super) fn build_pubmed_chunks(literature: &Literature) -> Vec<PubmedChunk> {
    let Some(pmid) = literature.pmid.as_deref() else {
        return Vec::new();
    };
    let document_id = format!("bio_literature:pubmed:PMID-{pmid}");
    chunk_text(&embeddable_text(literature), MAX_CHARS, OVERLAP_CHARS)
        .into_iter()
        .enumerate()
        .map(|(chunk_index, text)| {
            let chunk_id = format!("{document_id}:{chunk_index:04}");
            PubmedChunk {
                point_id: Uuid::new_v5(&Uuid::NAMESPACE_URL, chunk_id.as_bytes()).to_string(),
                chunk_id,
                chunk_index,
                text,
            }
        })
        .collect()
}

pub(super) fn pubmed_payload(
    literature: &Literature,
    chunk: &PubmedChunk,
    imported_at: &str,
) -> Value {
    let pmid = literature.pmid.as_deref().unwrap_or_default();
    let title = literature.title.as_deref().unwrap_or_default();
    json!({
        "document_id": format!("bio_literature:pubmed:PMID-{pmid}"),
        "chunk_id": chunk.chunk_id,
        "chunk_index": chunk.chunk_index,
        "category": "bio_literature",
        "source_type": "pubmed",
        "source_id": "pubmed",
        "source_uri": format!("https://pubmed.ncbi.nlm.nih.gov/{pmid}/"),
        "paper_id": format!("PMID:{pmid}"),
        "project_id": "pubmed",
        "title": title,
        "snippet": single_line(&chunk.text.chars().take(1_200).collect::<String>()),
        "text": chunk.text,
        "tags": ["pubmed"],
        "is_deleted": false,
        "imported_at": imported_at,
    })
}

fn embeddable_text(literature: &Literature) -> String {
    let mut fields = Vec::new();
    if let Some(title) = non_empty(literature.title.as_deref()) {
        fields.push(format!("Title: {title}"));
    }
    if let Some(abstract_text) = non_empty(literature.abstract_text.as_deref()) {
        fields.push(format!("Abstract: {abstract_text}"));
    }
    if let Some(mesh) = string_array(&literature.metadata, "mesh_terms")
        && !mesh.is_empty()
    {
        fields.push(format!("MeSH: {}", mesh.join("; ")));
    }
    if let Some(publication_types) = string_array(&literature.metadata, "publication_types")
        && !publication_types.is_empty()
    {
        fields.push(format!(
            "Publication types: {}",
            publication_types.join("; ")
        ));
    }
    if let Some(journal) = non_empty(literature.journal.as_deref()) {
        fields.push(format!("Journal: {journal}"));
    }
    if let Some(date) = non_empty(literature.publication_date.as_deref()) {
        fields.push(format!("Publication date: {date}"));
    }
    fields.join("\n")
}

fn string_array(metadata: &Value, key: &str) -> Option<Vec<String>> {
    metadata.get(key).and_then(Value::as_array).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(ToString::to_string)
            .collect()
    })
}

fn chunk_text(text: &str, max_chars: usize, overlap_chars: usize) -> Vec<String> {
    let chars: Vec<char> = normalize_text(text).chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = (start + max_chars).min(chars.len());
        if end < chars.len() {
            let boundary = [
                rfind(&chars, start, end, &['\n', '\n']),
                rfind(&chars, start, end, &['\n']),
                rfind(&chars, start, end, &['.', ' ']),
                rfind(&chars, start, end, &['。']),
            ]
            .into_iter()
            .flatten()
            .max();
            if let Some(boundary) = boundary
                && boundary > start + max_chars / 2
            {
                end = boundary + 1;
            }
        }
        let chunk = chars[start..end]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(overlap_chars);
    }
    chunks
}

fn rfind(chars: &[char], start: usize, end: usize, needle: &[char]) -> Option<usize> {
    if needle.is_empty() || end.saturating_sub(start) < needle.len() {
        return None;
    }
    (start..=end - needle.len())
        .rev()
        .find(|index| chars[*index..*index + needle.len()] == *needle)
}

fn normalize_text(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut normalized = String::new();
    let mut newline_run = 0;
    for ch in text.chars() {
        if ch == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                normalized.push(ch);
            }
        } else {
            newline_run = 0;
            normalized.push(ch);
        }
    }
    normalized.trim().to_string()
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn literature() -> Literature {
        Literature {
            literature_id: "id".to_string(),
            pmid: Some("12345678".to_string()),
            doi: Some("10.1000/example".to_string()),
            paper_id: Some("PMID:12345678".to_string()),
            title: Some("Example title".to_string()),
            abstract_text: Some("Example abstract.".to_string()),
            authors: vec!["Example Author".to_string()],
            journal: Some("Example Journal".to_string()),
            publication_date: Some("2026".to_string()),
            metadata: json!({
                "mesh_terms": ["Term A", "Term B"],
                "publication_types": ["Journal Article"],
            }),
        }
    }

    #[test]
    fn assembles_profile_fields_in_stable_order() {
        assert_eq!(
            embeddable_text(&literature()),
            "Title: Example title\nAbstract: Example abstract.\nMeSH: Term A; Term B\nPublication types: Journal Article\nJournal: Example Journal\nPublication date: 2026"
        );
    }

    #[test]
    fn generates_python_importer_compatible_point_id() {
        let chunks = build_pubmed_chunks(&literature());
        assert_eq!(chunks.len(), 1);
        assert_eq!(
            chunks[0].chunk_id,
            "bio_literature:pubmed:PMID-12345678:0000"
        );
        assert_eq!(chunks[0].point_id, "bfd25f52-4896-5082-af2e-5f9002793ea7");
    }

    #[test]
    fn collapses_newline_runs_and_uses_overlap() {
        assert_eq!(normalize_text("a\r\n\r\n\r\nb"), "a\n\nb");
        let text = format!("{}。{}", "a".repeat(1_100), "b".repeat(1_100));
        let chunks = chunk_text(&text, /*max_chars*/ 2_000, /*overlap_chars*/ 200);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].ends_with('。'));
        assert!(chunks[1].starts_with(&"a".repeat(199)));
    }
}
