use super::*;

const NCBI_EFETCH_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/efetch.fcgi";
const NCBI_ESEARCH_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi";
const NCBI_ESUMMARY_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi";

#[derive(Debug, Deserialize)]
pub(super) struct SearchPubmedLiteratureArgs {
    query: String,
    #[serde(default = "default_pubmed_retmax")]
    retmax: usize,
    #[serde(default = "default_pubmed_sort")]
    sort: PubmedSort,
    #[serde(default)]
    min_year: Option<u32>,
    #[serde(default)]
    max_year: Option<u32>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PubmedSort {
    Relevance,
    PubDate,
}

impl PubmedSort {
    fn as_esearch_sort(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::PubDate => "pub_date",
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct FetchPubmedRecordArgs {
    pmid: String,
    #[serde(default)]
    include_raw: bool,
    #[serde(default = "default_max_mesh_terms")]
    max_mesh_terms: usize,
}
fn default_pubmed_retmax() -> usize {
    10
}

fn default_pubmed_sort() -> PubmedSort {
    PubmedSort::Relevance
}

fn default_max_mesh_terms() -> usize {
    50
}
pub(super) async fn search_pubmed_literature(
    args: SearchPubmedLiteratureArgs,
) -> Result<String, FunctionCallError> {
    let query = args.query.trim();
    if query.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "PubMed query must not be empty".to_string(),
        ));
    }

    let retmax = args.retmax.clamp(1, 50);
    let client = BiomedExternalDbHandler::client()?;

    let mut esearch_query = ncbi_common_query();
    esearch_query.push(("db", "pubmed".to_string()));
    esearch_query.push(("term", query.to_string()));
    esearch_query.push(("retmax", retmax.to_string()));
    esearch_query.push(("retmode", "json".to_string()));
    esearch_query.push(("sort", args.sort.as_esearch_sort().to_string()));
    if let Some((min_year, max_year)) = pubmed_year_range(args.min_year, args.max_year)? {
        esearch_query.push(("datetype", "pdat".to_string()));
        esearch_query.push(("mindate", min_year.to_string()));
        esearch_query.push(("maxdate", max_year.to_string()));
    }

    let esearch_text = http_get_text_with_query(&client, NCBI_ESEARCH_URL, &esearch_query).await?;
    let esearch_json: Value = serde_json::from_str(&esearch_text).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse PubMed esearch JSON: {err}"))
    })?;
    if let Some(error) = esearch_json
        .pointer("/esearchresult/ERROR")
        .and_then(Value::as_str)
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "PubMed rejected the query: {error}"
        )));
    }
    let degradations = pubmed_query_degradations(&esearch_json);
    // PubMed's own reading of the query. It is the only honest record of what
    // was actually searched, since dropped qualifiers leave no other trace.
    let query_translation = esearch_json
        .pointer("/esearchresult/querytranslation")
        .and_then(Value::as_str);

    let pmids = esearch_json
        .pointer("/esearchresult/idlist")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // `count` is the total number of matches PubMed found, which is usually far
    // larger than the `retmax` slice we actually return.
    let total_count = esearch_json
        .pointer("/esearchresult/count")
        .and_then(Value::as_str)
        .and_then(|count| count.parse::<u64>().ok());

    if pmids.is_empty() {
        return to_pretty_json(&json!({
            "source": "PubMed",
            "url": NCBI_ESEARCH_URL,
            "query": query,
            "query_translation": query_translation,
            "query_degraded": degradations,
            "sort": args.sort.as_esearch_sort(),
            "year_range": pubmed_year_range_json(args.min_year, args.max_year),
            "retmax_effective": retmax,
            "total_count": total_count,
            "returned": 0,
            "results": [],
        }));
    }

    let mut esummary_query = ncbi_common_query();
    esummary_query.push(("db", "pubmed".to_string()));
    esummary_query.push(("id", pmids.join(",")));
    esummary_query.push(("retmode", "json".to_string()));
    let esummary_text =
        http_get_text_with_query(&client, NCBI_ESUMMARY_URL, &esummary_query).await?;
    let esummary_json: Value = serde_json::from_str(&esummary_text).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse PubMed esummary JSON: {err}"))
    })?;

    // Preserve the PMID order esearch returned; the esummary `result` map is
    // keyed by PMID and carries no ordering of its own.
    let results = pmids
        .iter()
        .filter_map(|pmid| {
            esummary_json
                .pointer("/result")
                .and_then(|result| result.get(pmid))
                .map(|entry| summarize_pubmed_summary(pmid, entry))
        })
        .collect::<Vec<_>>();
    // esearch found these PMIDs but esummary did not describe them. Report the
    // gap rather than letting `returned` quietly shrink.
    let dropped = pmids.len().saturating_sub(results.len());

    let result = json!({
        "source": "PubMed",
        "url": NCBI_ESEARCH_URL,
        "query": query,
        "query_translation": query_translation,
        "query_degraded": degradations,
        "sort": args.sort.as_esearch_sort(),
        "year_range": pubmed_year_range_json(args.min_year, args.max_year),
        "retmax_effective": retmax,
        "total_count": total_count,
        "returned": results.len(),
        "dropped_by_esummary": dropped,
        "results": results,
    });
    to_pretty_json(&result)
}

pub(super) async fn fetch_pubmed_record(
    args: FetchPubmedRecordArgs,
) -> Result<String, FunctionCallError> {
    let pmid = normalize_pmid(&args.pmid)?;
    let max_mesh_terms = args.max_mesh_terms.min(200);
    let client = BiomedExternalDbHandler::client()?;

    let mut query = ncbi_common_query();
    query.push(("db", "pubmed".to_string()));
    query.push(("id", pmid.clone()));
    query.push(("rettype", "medline".to_string()));
    query.push(("retmode", "text".to_string()));

    let raw_text = http_get_text_with_query(&client, NCBI_EFETCH_URL, &query).await?;
    if looks_like_ncbi_empty_response(&raw_text) {
        return Err(FunctionCallError::RespondToModel(format!(
            "NCBI returned no PubMed record for {pmid}: {}",
            raw_text.trim()
        )));
    }

    let fields = parse_medline_fields(&raw_text);
    if fields.is_empty() {
        return Err(FunctionCallError::RespondToModel(format!(
            "NCBI returned an unparsable MEDLINE record for {pmid}"
        )));
    }

    let mut mesh_terms = medline_all(&fields, "MH");
    let mesh_total = mesh_terms.len();
    mesh_terms.truncate(max_mesh_terms);

    let mut result = json!({
        "source": "PubMed",
        "url": NCBI_EFETCH_URL,
        "pmid": medline_first(&fields, "PMID").unwrap_or(pmid),
        "title": medline_first(&fields, "TI"),
        "abstract": medline_first(&fields, "AB"),
        "authors": medline_all(&fields, "FAU"),
        "journal": medline_first(&fields, "JT"),
        "journal_abbreviation": medline_first(&fields, "TA"),
        "publication_date": medline_first(&fields, "DP"),
        "doi": medline_doi(&fields),
        "publication_types": medline_all(&fields, "PT"),
        "mesh_terms": mesh_terms,
        "mesh_term_count": mesh_total,
        "language": medline_first(&fields, "LA"),
    });

    if args.include_raw {
        result["raw_record"] = Value::String(raw_text);
    }

    to_pretty_json(&result)
}
fn normalize_pmid(pmid: &str) -> Result<String, FunctionCallError> {
    let pmid = pmid.trim().trim_start_matches("PMID:").trim();
    if !pmid.is_empty() && pmid.chars().all(|ch| ch.is_ascii_digit()) {
        Ok(pmid.to_string())
    } else {
        Err(FunctionCallError::RespondToModel(
            "pmid must be a numeric PubMed identifier, for example `33301246`".to_string(),
        ))
    }
}

/// Report how PubMed quietly rewrote the query.
///
/// esearch answers 200 with no `ERROR` when it cannot honour part of a query:
/// an unknown field tag such as `CRISPR[Titel]` is dropped and silently widened
/// to an all-fields search, and an unknown `sort` falls back to the default
/// order. Both leave the caller believing a qualifier applied. The only signals
/// are the presence of an `errorlist` object (its inner arrays are empty in the
/// bad-tag case, so emptiness cannot be the test) and `warninglist`
/// `outputmessages`. Surface them so the model can distrust the result instead
/// of reading a widened search as a narrow one.
fn pubmed_query_degradations(esearch_json: &Value) -> Vec<String> {
    let mut notes = Vec::new();

    if let Some(errors) = esearch_json.pointer("/esearchresult/errorlist") {
        for (field, label) in [
            ("phrasesnotfound", "phrases not found"),
            ("fieldsnotfound", "field tags not found"),
        ] {
            match errors.get(field).and_then(Value::as_array) {
                Some(items) if !items.is_empty() => {
                    let joined = items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ");
                    notes.push(format!("{label}: {joined}"));
                }
                _ => {}
            }
        }
        if notes.is_empty() {
            notes.push(
                "PubMed reported an errorlist without details; part of the query may have been \
                 dropped and widened. Compare `query_translation` against the query you intended."
                    .to_string(),
            );
        }
    }

    if let Some(messages) = esearch_json
        .pointer("/esearchresult/warninglist/outputmessages")
        .and_then(Value::as_array)
    {
        notes.extend(
            messages
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string),
        );
    }

    notes
}

/// Echo the date window actually sent to PubMed, so a caller can tell an applied
/// filter from an absent one.
fn pubmed_year_range_json(min_year: Option<u32>, max_year: Option<u32>) -> Value {
    match (min_year, max_year) {
        (Some(min_year), Some(max_year)) => json!({
            "datetype": "pdat",
            "mindate": min_year,
            "maxdate": max_year,
        }),
        _ => Value::Null,
    }
}

/// PubMed's `mindate`/`maxdate` only take effect together, so treat a lone
/// bound as a caller error rather than silently dropping the filter.
fn pubmed_year_range(
    min_year: Option<u32>,
    max_year: Option<u32>,
) -> Result<Option<(u32, u32)>, FunctionCallError> {
    match (min_year, max_year) {
        (None, None) => Ok(None),
        (Some(min_year), Some(max_year)) if min_year <= max_year => Ok(Some((min_year, max_year))),
        (Some(min_year), Some(max_year)) => Err(FunctionCallError::RespondToModel(format!(
            "min_year ({min_year}) must not be greater than max_year ({max_year})"
        ))),
        _ => Err(FunctionCallError::RespondToModel(
            "min_year and max_year must be provided together".to_string(),
        )),
    }
}

/// Parse NCBI MEDLINE text into ordered `(tag, value)` pairs.
///
/// MEDLINE puts a four-column tag, a `-`, then the value; continuation lines are
/// indented and belong to the preceding tag. Tags repeat (authors, MeSH terms),
/// so order is preserved rather than collapsing into a map.
fn parse_medline_fields(text: &str) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(' ') {
            if let Some((_, value)) = fields.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        if let Some((tag, value)) = split_medline_line(line) {
            fields.push((tag, value));
        }
    }
    fields
}

fn split_medline_line(line: &str) -> Option<(String, String)> {
    // The separator sits at column 4 for every MEDLINE tag, which keeps values
    // containing `-` from being mistaken for the tag boundary.
    let separator = line.get(4..5)?;
    if separator != "-" {
        return None;
    }
    let tag = line.get(..4)?.trim();
    if tag.is_empty() {
        return None;
    }
    let value = line.get(5..).unwrap_or_default().trim();
    Some((tag.to_string(), value.to_string()))
}

fn medline_first(fields: &[(String, String)], tag: &str) -> Option<String> {
    fields
        .iter()
        .find(|(field_tag, _)| field_tag == tag)
        .map(|(_, value)| value.clone())
}

fn medline_all(fields: &[(String, String)], tag: &str) -> Vec<String> {
    fields
        .iter()
        .filter(|(field_tag, _)| field_tag == tag)
        .map(|(_, value)| value.clone())
        .collect()
}

/// MEDLINE reports article IDs as `<value> [<type>]`, so the DOI is whichever
/// `AID`/`LID` entry is tagged `[doi]`.
fn medline_doi(fields: &[(String, String)]) -> Option<String> {
    fields
        .iter()
        .filter(|(tag, _)| tag == "AID" || tag == "LID")
        .find_map(|(_, value)| {
            value
                .strip_suffix("[doi]")
                .map(|doi| doi.trim().to_string())
                .filter(|doi| !doi.is_empty())
        })
}

fn summarize_pubmed_summary(pmid: &str, entry: &Value) -> Value {
    let authors = entry
        .get("authors")
        .and_then(Value::as_array)
        .map(|authors| {
            authors
                .iter()
                .filter_map(|author| author.get("name").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let doi = entry
        .get("articleids")
        .and_then(Value::as_array)
        .and_then(|ids| {
            ids.iter()
                .find(|id| id.get("idtype").and_then(Value::as_str) == Some("doi"))
                .and_then(|id| id.get("value").and_then(Value::as_str))
        });

    json!({
        "pmid": pmid,
        "title": entry.get("title").and_then(Value::as_str),
        "authors": authors,
        "journal": entry.get("fulljournalname").and_then(Value::as_str),
        "publication_date": entry.get("pubdate").and_then(Value::as_str),
        "doi": doi,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const SAMPLE_MEDLINE: &str = "PMID- 33301246\nTI  - Integrated stress response in aging and\n      neurodegeneration.\nAB  - The integrated stress response (ISR) is a\n      conserved pathway.\nFAU - Smith, John A\nFAU - Doe, Jane\nJT  - Nature Reviews Neuroscience\nTA  - Nat Rev Neurosci\nDP  - 2020 Dec\nLA  - eng\nPT  - Journal Article\nPT  - Review\nMH  - Aging\nMH  - Neurodegenerative Diseases\nAID - 10.1038/s41583-020-00404-w [doi]\nAID - S1234-5678(20)30123-4 [pii]\n";
    #[test]
    fn parses_medline_tags_and_joins_continuation_lines() {
        let fields = parse_medline_fields(SAMPLE_MEDLINE);

        assert_eq!(medline_first(&fields, "PMID").as_deref(), Some("33301246"));
        // Continuation lines are indented and must fold into the previous tag.
        assert_eq!(
            medline_first(&fields, "TI").as_deref(),
            Some("Integrated stress response in aging and neurodegeneration.")
        );
        assert_eq!(
            medline_first(&fields, "AB").as_deref(),
            Some("The integrated stress response (ISR) is a conserved pathway.")
        );
        assert_eq!(medline_first(&fields, "DP").as_deref(), Some("2020 Dec"));
    }
    #[test]
    fn collects_repeated_medline_tags_in_order() {
        let fields = parse_medline_fields(SAMPLE_MEDLINE);

        assert_eq!(
            medline_all(&fields, "FAU"),
            vec!["Smith, John A".to_string(), "Doe, Jane".to_string()]
        );
        assert_eq!(
            medline_all(&fields, "MH"),
            vec![
                "Aging".to_string(),
                "Neurodegenerative Diseases".to_string()
            ]
        );
        assert_eq!(
            medline_all(&fields, "PT"),
            vec!["Journal Article".to_string(), "Review".to_string()]
        );
    }
    #[test]
    fn medline_doi_picks_the_doi_tagged_article_id() {
        let fields = parse_medline_fields(SAMPLE_MEDLINE);

        // Both AID lines are present; only the `[doi]` one may be selected.
        assert_eq!(
            medline_doi(&fields).as_deref(),
            Some("10.1038/s41583-020-00404-w")
        );
    }
    #[test]
    fn medline_doi_is_absent_when_no_doi_article_id() {
        let fields = parse_medline_fields("PMID- 1\nAID - S1234-5678(20)30123-4 [pii]\n");

        assert_eq!(medline_doi(&fields), None);
    }
    #[test]
    fn medline_values_containing_dashes_keep_the_column_four_separator() {
        let fields = parse_medline_fields("TI  - Anti-CD20 B-cell depletion.\n");

        assert_eq!(
            medline_first(&fields, "TI").as_deref(),
            Some("Anti-CD20 B-cell depletion.")
        );
    }
    #[test]
    fn normalizes_pmid_and_rejects_non_numeric() {
        assert_eq!(
            normalize_pmid(" 33301246 ").as_deref().ok(),
            Some("33301246")
        );
        assert_eq!(
            normalize_pmid("PMID:33301246").as_deref().ok(),
            Some("33301246")
        );
        assert!(normalize_pmid("33301246v2").is_err());
        assert!(normalize_pmid("").is_err());
    }
    #[test]
    fn pubmed_sort_uses_the_exact_strings_esearch_accepts() {
        // esearch answers 200 and silently falls back to the default order for an
        // unknown sort, so a typo here becomes a dead parameter with no signal.
        // PubMed's *website* uses `sort=pubdate` (no underscore) — the E-utilities
        // API does not. Pin the wire values.
        assert_eq!(PubmedSort::Relevance.as_esearch_sort(), "relevance");
        assert_eq!(PubmedSort::PubDate.as_esearch_sort(), "pub_date");
    }
    #[test]
    fn pubmed_year_range_json_echoes_only_a_complete_window() {
        assert_eq!(
            pubmed_year_range_json(Some(2015), Some(2020)),
            json!({"datetype": "pdat", "mindate": 2015, "maxdate": 2020})
        );
        assert_eq!(pubmed_year_range_json(None, None), Value::Null);
        assert_eq!(pubmed_year_range_json(Some(2015), None), Value::Null);
    }
    #[test]
    fn detects_dropped_field_tags_from_a_detail_free_errorlist() {
        // The bad-tag case PubMed actually returns: `errorlist` is present but its
        // arrays are empty, so emptiness cannot be the test.
        let notes = pubmed_query_degradations(&json!({
            "esearchresult": {
                "errorlist": {"phrasesnotfound": [], "fieldsnotfound": []},
                "querytranslation": "CRISPR[All Fields]",
            }
        }));

        assert_eq!(
            notes.len(),
            1,
            "a bare errorlist must still warn: {notes:?}"
        );
        assert!(
            notes[0].contains("query_translation"),
            "the warning points at the only reliable signal: {notes:?}"
        );
    }
    #[test]
    fn reports_named_missing_phrases_and_sort_warnings() {
        let notes = pubmed_query_degradations(&json!({
            "esearchresult": {
                "errorlist": {"phrasesnotfound": ["zzzznonsense"], "fieldsnotfound": []},
                "warninglist": {"outputmessages": ["Unknown sort schema 'pubdate' ignored"]},
            }
        }));

        assert_eq!(
            notes,
            vec![
                "phrases not found: zzzznonsense".to_string(),
                "Unknown sort schema 'pubdate' ignored".to_string(),
            ]
        );
    }
    #[test]
    fn clean_esearch_response_reports_no_degradation() {
        let notes = pubmed_query_degradations(&json!({
            "esearchresult": {"count": "42", "idlist": ["1"], "querytranslation": "CRISPR"}
        }));

        assert!(
            notes.is_empty(),
            "no false alarms on a clean query: {notes:?}"
        );
    }
    #[test]
    fn pubmed_year_range_requires_both_bounds_and_ordering() {
        assert_eq!(pubmed_year_range(None, None).ok(), Some(None));
        assert_eq!(
            pubmed_year_range(Some(2015), Some(2026)).ok(),
            Some(Some((2015, 2026)))
        );
        // A lone bound would silently drop the filter, so it is an error.
        assert!(pubmed_year_range(Some(2015), None).is_err());
        assert!(pubmed_year_range(None, Some(2026)).is_err());
        assert!(pubmed_year_range(Some(2026), Some(2015)).is_err());
    }
    #[test]
    fn summarizes_pubmed_esummary_entry() {
        let entry = json!({
            "uid": "33301246",
            "title": "Integrated stress response in aging.",
            "authors": [{"name": "Smith JA"}, {"name": "Doe J"}],
            "fulljournalname": "Nature Reviews Neuroscience",
            "pubdate": "2020 Dec",
            "articleids": [
                {"idtype": "pubmed", "value": "33301246"},
                {"idtype": "doi", "value": "10.1038/s41583-020-00404-w"}
            ],
        });

        assert_eq!(
            summarize_pubmed_summary("33301246", &entry),
            json!({
                "pmid": "33301246",
                "title": "Integrated stress response in aging.",
                "authors": ["Smith JA", "Doe J"],
                "journal": "Nature Reviews Neuroscience",
                "publication_date": "2020 Dec",
                "doi": "10.1038/s41583-020-00404-w",
            })
        );
    }
    #[tokio::test]
    #[ignore = "hits live NCBI PubMed E-utilities"]
    async fn biomed_external_db_live_searches_and_fetches_pubmed() {
        let search = search_pubmed_literature(SearchPubmedLiteratureArgs {
            query: "CRISPR".to_string(),
            retmax: 3,
            sort: PubmedSort::Relevance,
            min_year: Some(2015),
            max_year: Some(2020),
        })
        .await
        .expect("PubMed search should succeed");
        let search: Value = serde_json::from_str(&search).expect("PubMed search output is JSON");
        assert_eq!(search["source"], "PubMed");
        assert_eq!(search["returned"], 3);
        let results = search["results"]
            .as_array()
            .expect("PubMed results should be an array");
        assert_eq!(results.len(), 3);
        for result in results {
            assert!(
                result["pmid"].as_str().is_some_and(|pmid| {
                    !pmid.is_empty() && pmid.chars().all(|ch| ch.is_ascii_digit())
                }),
                "every result carries a numeric PMID: {result}"
            );
            assert!(
                result["title"].as_str().is_some_and(|t| !t.is_empty()),
                "every result carries a title: {result}"
            );
            // The year bounds must actually reach PubMed, not just ride along in
            // the arguments: assert every hit really falls inside the window.
            // `pubdate` looks like "2019 Apr 10" / "2020 Feb" / "2016", so the
            // leading four characters carry the year.
            let pubdate = result["publication_date"]
                .as_str()
                .unwrap_or_else(|| panic!("every result carries a publication_date: {result}"));
            let year: u32 = pubdate
                .get(..4)
                .and_then(|y| y.parse().ok())
                .unwrap_or_else(|| panic!("publication_date starts with a year: {pubdate:?}"));
            assert!(
                (2015..=2020).contains(&year),
                "min_year/max_year must filter for real; got {year} from {pubdate:?}"
            );
        }

        // A well-known, stable record: the Jinek 2012 CRISPR paper.
        let record = fetch_pubmed_record(FetchPubmedRecordArgs {
            pmid: "22745249".to_string(),
            include_raw: false,
            max_mesh_terms: 50,
        })
        .await
        .expect("PubMed record 22745249 should fetch");
        let record: Value = serde_json::from_str(&record).expect("PubMed record output is JSON");
        assert_eq!(record["pmid"], "22745249");
        assert_eq!(
            record["doi"], "10.1126/science.1225829",
            "DOI is parsed from the `[doi]`-tagged AID line"
        );
        assert!(
            record["title"]
                .as_str()
                .is_some_and(|title| title.contains("dual-RNA-guided DNA endonuclease")),
            "title parses, keeping hyphens that are not tag separators: {}",
            record["title"]
        );
        assert!(
            record["abstract"]
                .as_str()
                .is_some_and(|abstract_text| abstract_text.len() > 200),
            "abstract continuation lines should be folded into one value"
        );
        assert!(
            !record["authors"]
                .as_array()
                .expect("authors should be an array")
                .is_empty(),
            "repeated FAU tags should be collected"
        );
    }
    #[tokio::test]
    #[ignore = "hits live NCBI PubMed E-utilities"]
    async fn biomed_external_db_live_surfaces_a_silently_widened_query() {
        // `[Titel]` is a typo for `[Title]`. PubMed answers 200 with no ERROR and
        // quietly drops the qualifier, widening to an all-fields search — the same
        // shape of failure as a filter that never reaches the backend. The result
        // must carry that fact.
        let widened = search_pubmed_literature(SearchPubmedLiteratureArgs {
            query: "CRISPR[Titel]".to_string(),
            retmax: 1,
            sort: PubmedSort::Relevance,
            min_year: None,
            max_year: None,
        })
        .await
        .expect("a bad field tag is not a hard error for PubMed");
        let widened: Value = serde_json::from_str(&widened).expect("output is JSON");

        let degraded = widened["query_degraded"]
            .as_array()
            .expect("query_degraded is always an array");
        assert!(
            !degraded.is_empty(),
            "a dropped field tag must be reported, not silently widened: {widened}"
        );
        assert!(
            widened["query_translation"]
                .as_str()
                .is_some_and(|t| !t.contains("Titel")),
            "query_translation shows PubMed dropped the tag: {}",
            widened["query_translation"]
        );

        // A well-formed tag must not trip the warning.
        let clean = search_pubmed_literature(SearchPubmedLiteratureArgs {
            query: "CRISPR[Title]".to_string(),
            retmax: 1,
            sort: PubmedSort::Relevance,
            min_year: None,
            max_year: None,
        })
        .await
        .expect("a valid field tag searches cleanly");
        let clean: Value = serde_json::from_str(&clean).expect("output is JSON");
        assert_eq!(
            clean["query_degraded"].as_array().map(Vec::len),
            Some(0),
            "no false alarm on a valid tag: {clean}"
        );
        assert!(
            clean["total_count"].as_u64() < widened["total_count"].as_u64(),
            "the honoured tag really narrows the search: {} vs {}",
            clean["total_count"],
            widened["total_count"]
        );
    }
    #[tokio::test]
    #[ignore = "hits live NCBI PubMed E-utilities"]
    async fn biomed_external_db_live_rejects_bad_pubmed_pmid() {
        // PMID 0 does not exist; NCBI answers with an error body rather than a
        // non-2xx status, so the empty-response guard must catch it.
        let error = fetch_pubmed_record(FetchPubmedRecordArgs {
            pmid: "0".to_string(),
            include_raw: false,
            max_mesh_terms: 50,
        })
        .await
        .expect_err("PMID 0 should not resolve");

        assert!(
            matches!(error, FunctionCallError::RespondToModel(_)),
            "a bad PMID is a model-facing error, not a fatal one: {error:?}"
        );
    }
}
