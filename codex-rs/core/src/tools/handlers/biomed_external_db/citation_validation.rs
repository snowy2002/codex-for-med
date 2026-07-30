use super::*;

const CROSSREF_WORKS_BASE: &str = "https://api.crossref.org/works";
/// One Crossref request per citation; cap the batch so a single call cannot
/// hammer the API.
const MAX_CITATIONS_PER_CALL: usize = 25;

#[derive(Debug, Deserialize)]
pub(super) struct ValidateCitationsArgs {
    citations: Vec<CitationToCheck>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CitationToCheck {
    pub(crate) doi: String,
    #[serde(default)]
    pub(crate) claimed_title: Option<String>,
    #[serde(default)]
    pub(crate) claimed_authors: Vec<String>,
    #[serde(default)]
    pub(crate) claimed_year: Option<u64>,
}
pub(super) async fn validate_citations(
    args: ValidateCitationsArgs,
) -> Result<String, FunctionCallError> {
    if args.citations.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "citations must contain at least one entry".to_string(),
        ));
    }
    // CrossRef publishes a rate limit; keep well under it and identify the tool
    // in the User-Agent, the same etiquette the NCBI tools follow.
    if args.citations.len() > MAX_CITATIONS_PER_CALL {
        return Err(FunctionCallError::RespondToModel(format!(
            "at most {MAX_CITATIONS_PER_CALL} citations may be validated per call, got {}",
            args.citations.len()
        )));
    }

    let client = BiomedExternalDbHandler::client()?;
    let mut results = Vec::with_capacity(args.citations.len());
    let mut mismatches = 0usize;
    let mut unresolved = 0usize;

    for citation in &args.citations {
        let verdict = validate_one_citation(&client, citation).await?;
        match verdict.get("status").and_then(Value::as_str) {
            Some("mismatch") => mismatches += 1,
            Some("doi_not_found") | Some("invalid_doi") => unresolved += 1,
            _ => {}
        }
        results.push(verdict);
    }

    let result = json!({
        "source": "Crossref",
        "checked": results.len(),
        "mismatches": mismatches,
        "unresolved": unresolved,
        "results": results,
    });
    to_pretty_json(&result)
}

/// Validate a single citation against the authoritative Crossref record.
///
/// This is the deterministic half of citation review: resolve the DOI and check
/// whether it points to the article the citation claims. It does NOT judge
/// whether the article supports the surrounding claim — that needs reading the
/// paper and belongs to a reviewer agent, not this tool.
pub(crate) async fn validate_one_citation(
    client: &reqwest::Client,
    citation: &CitationToCheck,
) -> Result<Value, FunctionCallError> {
    let doi = normalize_doi(&citation.doi);
    if doi.is_empty() {
        return Ok(json!({
            "doi": citation.doi,
            "status": "invalid_doi",
            "detail": "empty or unparseable DOI",
        }));
    }

    // Crossref etiquette: identify via mailto so we ride the "polite pool".
    let url = format!("{CROSSREF_WORKS_BASE}/{doi}");
    let query = match std::env::var("CROSSREF_MAILTO") {
        Ok(email) if !email.trim().is_empty() => vec![("mailto", email)],
        _ => Vec::new(),
    };

    let response = client.get(&url).query(&query).send().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("Crossref request for {doi} failed: {err}"))
    })?;
    let status = response.status();
    if status.as_u16() == 404 {
        return Ok(json!({
            "doi": doi,
            "status": "doi_not_found",
            "detail": "Crossref has no record for this DOI",
        }));
    }
    let body = response.text().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "failed to read Crossref response for {doi}: {err}"
        ))
    })?;
    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "Crossref returned {status} for {doi}: {}",
            truncate_for_error(&body)
        )));
    }

    let parsed: Value = serde_json::from_str(&body).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse Crossref JSON for {doi}: {err}"))
    })?;
    let message = parsed.get("message").unwrap_or(&Value::Null);

    let actual_title = message
        .get("title")
        .and_then(Value::as_array)
        .and_then(|titles| titles.first())
        .and_then(Value::as_str);
    let actual_authors = crossref_authors(message);
    let actual_year = message
        .pointer("/issued/date-parts/0/0")
        .and_then(Value::as_u64);

    let title_match = citation
        .claimed_title
        .as_deref()
        .map(|claimed| titles_match(claimed, actual_title.unwrap_or("")));
    let authors_match = if citation.claimed_authors.is_empty() {
        None
    } else {
        Some(
            citation
                .claimed_authors
                .iter()
                .all(|claimed| author_present(claimed, &actual_authors)),
        )
    };
    let year_match = citation
        .claimed_year
        .zip(actual_year)
        .map(|(claimed, actual)| claimed == actual);

    // DOI resolution establishes the record identity. A supplied title is the
    // primary metadata consistency check and publication year disambiguates
    // works with identical titles. Author names are supporting evidence because
    // ordering, initials, transliteration, and group authorship vary between
    // bibliographic providers. Fall back to authors only when neither title nor
    // year was supplied.
    let is_mismatch = citation_metadata_mismatch(title_match, year_match, authors_match);

    Ok(json!({
        "doi": doi,
        "status": if is_mismatch { "mismatch" } else { "verified" },
        "resolved_title": actual_title,
        "resolved_authors": actual_authors,
        "resolved_year": actual_year,
        "title_match": title_match,
        "year_match": year_match,
        "authors_match": authors_match,
    }))
}

fn citation_metadata_mismatch(
    title_match: Option<bool>,
    year_match: Option<bool>,
    authors_match: Option<bool>,
) -> bool {
    title_match == Some(false)
        || year_match == Some(false)
        || (title_match.is_none() && year_match.is_none() && authors_match == Some(false))
}

/// Strip the many ways a DOI arrives (a `doi:` prefix, a resolver URL) down to
/// the bare identifier, lowercased since DOIs are case-insensitive.
fn normalize_doi(raw: &str) -> String {
    let mut doi = raw.trim();
    for prefix in [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi:",
        "DOI:",
    ] {
        if let Some(rest) = doi.strip_prefix(prefix) {
            doi = rest.trim();
            break;
        }
    }
    // A real DOI always starts with the `10.` registrant prefix.
    if doi.starts_with("10.") {
        doi.to_ascii_lowercase()
    } else {
        String::new()
    }
}

/// Compare two titles tolerantly. Crossref titles carry typographic characters
/// (en-dashes, smart quotes, trailing punctuation, case and spacing differences)
/// that a raw string equality would wrongly flag as a mismatch.
fn titles_match(a: &str, b: &str) -> bool {
    normalize_for_compare(a) == normalize_for_compare(b)
}

fn normalize_for_compare(text: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    for ch in text.chars() {
        let c = match ch {
            // Fold the dash and quote variants Crossref emits.
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{02bc}' => '\'',
            '\u{201c}' | '\u{201d}' => '"',
            other => other,
        };
        if c.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else if c.is_alphanumeric() || matches!(c, '-' | '\'' | '"' | ':' | '.') {
            out.extend(c.to_lowercase());
            prev_space = false;
        }
    }
    while out.ends_with([' ', '.', ':']) {
        out.pop();
    }
    out
}

/// A claimed author matches if their surname appears in any Crossref author
/// name. Citation styles vary too much (initials vs full given names, ordering)
/// for exact equality; the family name is the stable anchor.
fn author_present(claimed: &str, actual: &[String]) -> bool {
    let claimed_family = claimed
        .split_once(',')
        .map_or(claimed, |(family, _given)| family);
    let claimed_family_norm = normalize_for_compare(claimed_family);
    let claimed_surname = if claimed.contains(',') {
        claimed_family_norm.as_str()
    } else {
        claimed_family_norm
            .rsplit(' ')
            .next()
            .unwrap_or(&claimed_family_norm)
    };
    if claimed_surname.is_empty() {
        return false;
    }
    actual.iter().any(|name| {
        let name_norm = normalize_for_compare(name);
        name_norm == claimed_surname
            || name_norm
                .strip_suffix(claimed_surname)
                .is_some_and(|given| given.ends_with(' '))
    })
}

fn crossref_authors(message: &Value) -> Vec<String> {
    message
        .get("author")
        .and_then(Value::as_array)
        .map(|authors| {
            authors
                .iter()
                .filter_map(|a| {
                    let family = a.get("family").and_then(Value::as_str);
                    let given = a.get("given").and_then(Value::as_str);
                    match (given, family) {
                        (Some(g), Some(f)) => Some(format!("{g} {f}")),
                        (None, Some(f)) => Some(f.to_string()),
                        _ => a
                            .get("name")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    #[test]
    fn normalize_doi_strips_prefixes_and_lowercases() {
        assert_eq!(
            normalize_doi("10.1126/Science.1225829"),
            "10.1126/science.1225829"
        );
        assert_eq!(
            normalize_doi("https://doi.org/10.1038/nature"),
            "10.1038/nature"
        );
        assert_eq!(normalize_doi(" doi:10.1000/xyz "), "10.1000/xyz");
        assert_eq!(normalize_doi("DOI: 10.1000/abc"), "10.1000/abc");
    }
    #[test]
    fn normalize_doi_rejects_non_doi() {
        assert_eq!(normalize_doi("not-a-doi"), "");
        assert_eq!(normalize_doi(""), "");
        assert_eq!(normalize_doi("PMID:12345"), "");
    }
    #[test]
    fn titles_match_tolerates_typographic_variants() {
        // Crossref uses an en-dash and title case; a citation may use a hyphen
        // and different spacing. These are the same title.
        assert!(titles_match(
            "A programmable dual-RNA-guided DNA endonuclease",
            "A Programmable Dual-RNA\u{2013}Guided DNA Endonuclease",
        ));
        assert!(titles_match("Foo:  bar.", "foo: bar"));
    }
    #[test]
    fn titles_match_rejects_genuinely_different_titles() {
        assert!(!titles_match(
            "CRISPR gene editing in human cells",
            "A programmable dual-RNA-guided DNA endonuclease",
        ));
    }
    #[test]
    fn author_present_matches_by_surname() {
        let actual = vec!["Martin Jinek".to_string(), "Jennifer A. Doudna".to_string()];
        // Surname anchors the match despite given-name style differences.
        assert!(author_present("Jinek", &actual));
        assert!(author_present("M Jinek", &actual));
        assert!(author_present("Jinek, Martin", &actual));
        assert!(author_present("Jennifer Doudna", &actual));
        assert!(!author_present("Zhang", &actual));
        assert!(!author_present("", &actual));
    }
    #[test]
    fn author_present_matches_pubmed_family_given_format() {
        let actual = vec![
            "Fernando P. Polack".to_string(),
            "Stephen J. Thomas".to_string(),
            "Nicholas Kitchin".to_string(),
            "Gonzalo Perez Marc".to_string(),
        ];

        assert!(author_present("Polack, Fernando P", &actual));
        assert!(author_present("Thomas, Stephen J", &actual));
        assert!(author_present("Kitchin, Nicholas", &actual));
        assert!(author_present("Perez Marc, Gonzalo", &actual));
        assert!(!author_present("Other, Fernando P", &actual));
    }
    #[test]
    fn citation_metadata_uses_title_and_year_before_authors() {
        assert_eq!(
            [
                citation_metadata_mismatch(Some(true), Some(true), Some(false)),
                citation_metadata_mismatch(Some(false), Some(true), Some(true)),
                citation_metadata_mismatch(Some(true), Some(false), Some(true)),
                citation_metadata_mismatch(None, Some(true), Some(false)),
                citation_metadata_mismatch(None, None, Some(false)),
                citation_metadata_mismatch(None, None, Some(true)),
                citation_metadata_mismatch(None, None, None),
            ],
            [false, true, true, false, true, false, false]
        );
    }
    #[test]
    fn crossref_authors_handles_given_family_and_collective() {
        let msg = json!({
            "author": [
                {"given": "Martin", "family": "Jinek", "sequence": "first"},
                {"family": "Doudna"},
                {"name": "The CRISPR Consortium"}
            ]
        });
        assert_eq!(
            crossref_authors(&msg),
            vec![
                "Martin Jinek".to_string(),
                "Doudna".to_string(),
                "The CRISPR Consortium".to_string(),
            ]
        );
    }
    #[tokio::test]
    #[ignore = "hits live Crossref API"]
    async fn biomed_external_db_live_validates_citations_against_crossref() {
        // Mixed batch: a correct citation, a title that resolves to a different
        // paper (the core failure this tool exists to catch), and a DOI that
        // does not exist at all.
        let out = validate_citations(ValidateCitationsArgs {
            citations: vec![
                CitationToCheck {
                    doi: "10.1126/science.1225829".to_string(),
                    claimed_title: Some(
                        "A programmable dual-RNA-guided DNA endonuclease in adaptive bacterial immunity"
                            .to_string(),
                    ),
                    claimed_authors: vec!["Jinek".to_string(), "Doudna".to_string()],
                    claimed_year: Some(2012),
                },
                CitationToCheck {
                    doi: "10.1126/science.1225829".to_string(),
                    claimed_title: Some("A totally unrelated paper about protein folding".to_string()),
                    claimed_authors: vec![],
                    claimed_year: Some(2012),
                },
                CitationToCheck {
                    doi: "10.9999/this-doi-does-not-exist-xyz".to_string(),
                    claimed_title: None,
                    claimed_authors: vec![],
                    claimed_year: None,
                },
            ],
        })
        .await
        .expect("validation should complete");
        let out: Value = serde_json::from_str(&out).expect("output is JSON");

        assert_eq!(out["checked"], 3);
        assert_eq!(out["mismatches"], 1);
        assert_eq!(out["unresolved"], 1);

        let r = out["results"].as_array().expect("results is an array");
        assert_eq!(
            r[0]["status"], "verified",
            "correct citation verifies: {}",
            r[0]
        );
        assert_eq!(r[0]["title_match"], true);
        assert_eq!(r[0]["authors_match"], true);
        assert_eq!(
            r[1]["status"], "mismatch",
            "a wrong title for a real DOI is caught: {}",
            r[1]
        );
        assert_eq!(r[1]["title_match"], false);
        assert_eq!(
            r[2]["status"], "doi_not_found",
            "a nonexistent DOI is reported, not errored: {}",
            r[2]
        );
    }
}
