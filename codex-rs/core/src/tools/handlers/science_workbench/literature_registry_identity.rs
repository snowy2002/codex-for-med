//! Conservative identifier and metadata normalization shared by registry paths.

use serde_json::Value;
use serde_json::json;
use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;

use super::literature_registry::LiteratureInput;

pub(super) fn normalize_input(mut input: LiteratureInput) -> LiteratureInput {
    input.pmid = input.pmid.as_deref().and_then(normalize_pmid);
    input.doi = input.doi.as_deref().and_then(normalize_doi);
    input.paper_id = input.paper_id.as_deref().and_then(normalize_paper_id);
    if let Some(pmid_from_paper) = input
        .paper_id
        .as_deref()
        .and_then(|paper_id| paper_id.strip_prefix("PMID:"))
        .map(str::to_string)
    {
        input.pmid.get_or_insert(pmid_from_paper);
    }
    input.title = non_empty(input.title);
    input.abstract_text = non_empty(input.abstract_text);
    input.journal = non_empty(input.journal);
    input.publication_date = non_empty(input.publication_date);
    input.authors = input
        .authors
        .into_iter()
        .filter_map(|author| non_empty(Some(author)))
        .collect();
    input
}

pub(super) fn normalize_pmid(value: &str) -> Option<String> {
    let mut value = value.trim();
    if let Some(stripped) = strip_ascii_prefix(value, "PMID:") {
        value = stripped.trim();
    }
    for prefix in [
        "https://pubmed.ncbi.nlm.nih.gov/",
        "http://pubmed.ncbi.nlm.nih.gov/",
    ] {
        if let Some(stripped) = strip_ascii_prefix(value, prefix) {
            value = stripped.trim_matches('/');
            break;
        }
    }
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        value
            .parse::<u64>()
            .ok()
            .filter(|pmid| *pmid > 0)
            .map(|pmid| pmid.to_string())
    } else {
        None
    }
}

pub(super) fn normalize_doi(value: &str) -> Option<String> {
    let mut value = value.trim();
    for prefix in [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi:",
    ] {
        if let Some(stripped) = strip_ascii_prefix(value, prefix) {
            value = stripped.trim();
            break;
        }
    }
    let decoded = percent_decode(value)?;
    let normalized = decoded.trim().to_lowercase();
    if normalized.starts_with("10.")
        && normalized.contains('/')
        && !normalized.contains(char::is_whitespace)
    {
        Some(normalized)
    } else {
        None
    }
}

pub(super) fn normalize_paper_id(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(pmid) = normalize_pmid(value) {
        return Some(format!("PMID:{pmid}"));
    }
    if let Some(patent) = normalize_known_patent(value) {
        return Some(patent);
    }
    Some(value.to_string())
}

fn normalize_known_patent(value: &str) -> Option<String> {
    let compact: String = value
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .collect();
    let uppercase = compact.to_ascii_uppercase();
    if ["EP", "US", "WO", "CN", "JP"]
        .iter()
        .any(|prefix| uppercase.starts_with(prefix))
        && uppercase.chars().any(|ch| ch.is_ascii_digit())
    {
        Some(uppercase)
    } else {
        None
    }
}

pub(super) fn identifiers_from_source_uri(uri: &str) -> (Option<String>, Option<String>) {
    let direct = (normalize_pmid(uri), normalize_doi(uri));
    if direct.0.is_some() || direct.1.is_some() {
        return direct;
    }
    let Ok(parsed) = url::Url::parse(uri) else {
        return (None, None);
    };
    let host = parsed.host_str().unwrap_or_default();
    let decoded_path = percent_decode(parsed.path()).unwrap_or_else(|| parsed.path().to_string());
    if host.eq_ignore_ascii_case("pubmed.ncbi.nlm.nih.gov") {
        return (
            decoded_path
                .trim_matches('/')
                .split('/')
                .next()
                .and_then(normalize_pmid),
            None,
        );
    }
    if matches!(host.to_ascii_lowercase().as_str(), "doi.org" | "dx.doi.org") {
        return (None, normalize_doi(decoded_path.trim_start_matches('/')));
    }
    (None, None)
}

pub(super) fn paper_id_from_source_uri(uri: &str) -> Option<String> {
    let decoded = percent_decode(uri).unwrap_or_else(|| uri.to_string());
    decoded
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .find_map(normalize_known_patent)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = bytes.get(index + 1).copied().and_then(hex_value)?;
            let low = bytes.get(index + 2).copied().and_then(hex_value)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn strip_ascii_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .and_then(|_| value.get(prefix.len()..))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

pub(super) fn normalize_title(value: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;
    for ch in value.nfkc().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            if pending_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push(ch);
            pending_space = false;
        } else {
            pending_space = true;
        }
    }
    normalized
}

pub(super) fn normalized_first_author(authors: &[String]) -> Option<String> {
    authors
        .first()
        .map(|author| normalize_title(author))
        .filter(|author| !author.is_empty())
}

pub(super) fn publication_year(publication_date: Option<&str>) -> Option<i32> {
    let date = publication_date?;
    date.as_bytes()
        .windows(4)
        .find(|window| window.iter().all(u8::is_ascii_digit))
        .and_then(|window| std::str::from_utf8(window).ok())
        .and_then(|year| year.parse::<i32>().ok())
}

pub(super) fn title_jaccard(left: &str, right: &str) -> f64 {
    let left = normalize_title(left)
        .split_whitespace()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let right = normalize_title(right)
        .split_whitespace()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(&right).count();
    let union = left.union(&right).count();
    intersection as f64 / union as f64
}

pub(super) fn should_replace_field(
    existing_metadata: &Value,
    incoming_metadata: &Value,
    field: &str,
    existing_present: bool,
    incoming_present: bool,
) -> bool {
    if !incoming_present {
        return false;
    }
    if !existing_present {
        return true;
    }
    let existing_source = existing_metadata
        .pointer(&format!("/field_sources/{field}/source"))
        .and_then(Value::as_str);
    let incoming_source = incoming_metadata
        .pointer(&format!("/field_sources/{field}/source"))
        .and_then(Value::as_str);
    let source_priority = |source: Option<&str>| match source {
        Some(source) if source.eq_ignore_ascii_case("pubmed") => 100,
        Some(_) => 10,
        None => 0,
    };
    if source_priority(incoming_source) != source_priority(existing_source) {
        return source_priority(incoming_source) > source_priority(existing_source);
    }
    if incoming_source != existing_source {
        return false;
    }
    let observed_at = |metadata: &Value| {
        metadata
            .pointer(&format!("/field_sources/{field}/observed_at"))
            .and_then(Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
    };
    observed_at(incoming_metadata)
        .zip(observed_at(existing_metadata))
        .is_some_and(|(incoming, existing)| incoming > existing)
}

pub(super) fn update_field_source(metadata: &mut Value, incoming_metadata: &Value, field: &str) {
    let Some(incoming) = incoming_metadata
        .pointer(&format!("/field_sources/{field}"))
        .cloned()
    else {
        return;
    };
    let Some(field_sources) = metadata
        .get_mut("field_sources")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    field_sources.insert(field.to_string(), incoming);
}

pub(super) fn metadata_with_defaults(metadata: Value) -> Value {
    let defaults = json!({
        "raw_identifiers": {},
        "field_sources": {},
        "mesh_terms": [],
        "publication_types": [],
        "language": null,
        "relations": [],
        "flags": {
            "abstract_missing": false,
            "retracted": false,
            "corrected": false
        }
    });
    let mut metadata = if metadata.is_object() {
        metadata
    } else {
        json!({})
    };
    merge_json_missing(&mut metadata, defaults);
    metadata
}

pub(super) fn merge_json_missing(existing: &mut Value, incoming: Value) {
    if existing.is_null() {
        *existing = incoming;
        return;
    }
    match incoming {
        Value::Object(incoming) => {
            let Value::Object(existing) = existing else {
                return;
            };
            for (key, value) in incoming {
                match existing.get_mut(&key) {
                    Some(existing_value) => merge_json_missing(existing_value, value),
                    None => {
                        existing.insert(key, value);
                    }
                }
            }
        }
        Value::Array(incoming) => {
            if let Value::Array(existing) = existing
                && existing.is_empty()
            {
                *existing = incoming;
            }
        }
        _ => {}
    }
}
