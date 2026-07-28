---
name: codex-med-science-workbench
description: Route and chain codex-med biomedical retrieval, literature-map, citation-validation, review-resolution, and PubMed vector-reconciliation tools into reproducible research workflows. Use when the user asks for quick PubMed evidence, a durable local or PubMed literature map, comparison of local knowledge with public literature, citation verification, ingestion of retrieved PubMed records into Qdrant, repair of incomplete vector coverage, resolution of possible-duplicate literature records, or inspection of available medical databases.
---

# Codex Med Science Workbench

Choose the smallest workflow that produces the artifact the user wants. Keep all
durable steps for one project in the same workspace so they share the literature
registry and project manifest.

## Route by Intended Result

- Use `list_med_knowledge_collections` to discover available SQL and vector
  backends when the source is unknown.
- Use `describe_med_database` to inspect schemas, payload fields, row counts, or
  collection metadata before constructing a structured query.
- Use `search_vector_knowledge` for a quick semantic lookup from local knowledge
  without writing project files.
- Use `search_pubmed_literature` for a quick PubMed result list when PMIDs are
  unknown and no durable project is requested.
- Use `fetch_pubmed_record` to fetch the abstract and MeSH terms of a selected
  PMID.
- Use `validate_citations` to verify DOI, title, and author metadata against
  Crossref. Do not treat this metadata check as proof that a paper supports a
  scientific claim.
- Use `literature_map` to create a durable evidence package from the local
  codex-med vector collection.
- Use `pubmed_literature_map` to create a durable PubMed package, register
  canonical literature identities, and optionally ingest complete records into
  Qdrant.
- Use `resolve_literature_review` only after a human decides whether a
  possible-duplicate pair is the same publication.
- Use `reconcile_pubmed_vectors` to verify or repair vector coverage for PubMed
  records already present in the workspace registry.

For a short answer in chat, do not create a project unless the user asks for
saved files, a literature map, provenance, an export, or a reusable workflow.

## Chain Quick PubMed Evidence

Use this sequence for a small answer without project files:

1. Call `search_pubmed_literature`.
2. Inspect `query_degraded` and `query_translation`. Correct and rerun the query
   if PubMed dropped or widened a qualifier.
3. Select relevant PMIDs from the returned metadata.
4. Call `fetch_pubmed_record` only for records whose abstracts or MeSH terms are
   needed.
5. Call `validate_citations` for DOI-bearing references when citation identity
   matters.
6. Separate verified metadata from the model's assessment of claim support in
   the final answer.

Do not fetch every record by default. Keep discovery broad and evidence
extraction selective.

## Build a Durable Literature Project

### Local knowledge

1. Discover or describe the backend only when its contents are unfamiliar.
2. Call `literature_map` with a clear `topic`, a stable `project_id`, and the
   requested `top_k`.
3. Inspect `literature/local/literature_ids.csv`, `report.md`, the immutable run
   snapshot, and provenance before summarizing.

### PubMed

1. Call `pubmed_literature_map` directly with a clear `topic` and stable
   `project_id`.
2. Supply `pubmed_query` for Boolean expressions, field tags, author or journal
   filters, and exact PMID searches.
3. Set `min_year` and `max_year` together. Use `sort: "pub_date"` for recent
   papers and relevance otherwise.
4. Keep `fetch_abstracts` enabled for evidence work. Disable it only for a fast
   metadata inventory.
5. Enable `validate_citations` when DOI metadata must be checked in the same
   durable run.
6. Inspect `fetch_errors`, `citation_validation`, `query_degraded` in
   provenance, `vector_statuses`, and `incomplete_vectors`.
7. Review `literature/pubmed/literature_ids.csv`, `report.md`, citations,
   snapshot, provenance, and the merged project manifest.

Do not manually call `search_pubmed_literature` and `fetch_pubmed_record` before
`pubmed_literature_map` unless previewing the query is necessary. The map tool
already performs search, detail fetching, registration, artifact creation,
citation validation, and vector-ingestion bookkeeping.

### Compare local knowledge with PubMed

1. Use one stable `project_id`.
2. Run `literature_map` for local curated evidence.
3. Run `pubmed_literature_map` for public evidence.
4. Compare canonical outputs under `literature/local/` and
   `literature/pubmed/`.
5. Report overlap, source-specific records, ranking differences, and search
   limitations. Do not merge the two ranked CSV files manually; let the shared
   registry and project manifest preserve identity and provenance.

## Ingest Retrieved PubMed Literature

Treat vector ingestion as an explicit state-changing workflow.

1. Confirm that the user requested or authorized vector writes.
2. Start Codex Med with these runtime settings:
   - `CODEX_MED_PUBMED_VECTOR_WRITES=1`
   - an explicit `CODEX_MED_VECTOR_COLLECTION`
   - `CODEX_MED_VECTOR_QDRANT_API_KEY` when Qdrant requires authentication
   - `CODEX_MED_EMBEDDING_API_KEY` when the embedding service requires
     authentication
3. Refuse to test against the production collection when a disposable
   collection is intended.
4. Before the first authorized write to the default production collection, run
   `literature_map` once in the same workspace with
   `CODEX_MED_INITIALIZE_LITERATURE_BASELINE=1`. Inspect
   `baseline_initialization` in provenance, then remove the initialization
   flag. Do not bypass a missing or invalid production baseline.
5. Call `pubmed_literature_map` with `require_vector_complete: true` when the
   task requires an ingestion guarantee.
6. Declare success only when `vector_complete` is true and every record has
   status `complete` or `already_vectorized`.
7. Treat `vector_write_disabled`, `possible_duplicate`, `blocked_conflict`,
   `failed`, missing points, or any other status as incomplete even if
   literature files were created.

Keep vector writes disabled for ordinary retrieval and literature-map requests.
Never place secret values in project artifacts, prompts, reports, or provenance.

## Recover Incomplete Ingestion

Use the returned status to choose the next tool:

- For `possible_duplicate`, `blocked_conflict`, or a `review_case_id`, show the
  conflicting identities to the user and request an explicit same/different
  decision. Call `resolve_literature_review` with the human rationale; never
  infer approval from title or identifier similarity.
- After resolving a review, call `reconcile_pubmed_vectors` for the affected
  canonical literature ID.
- For failed, stale, or missing vectors without an identity conflict, call
  `reconcile_pubmed_vectors` with the affected `literature_ids` first. Omit IDs
  only when the user requests a workspace-wide batch audit.
- Inspect `complete`, `statuses`, `expected_points`, `verified_points`,
  `verification_method`, and per-record errors after reconciliation.
- Rerun the map only to refresh search results or regenerate project artifacts;
  do not use repeated map runs as a substitute for reconciliation.

Keep `pubmed_literature_map`, `resolve_literature_review`, and
`reconcile_pubmed_vectors` in the same working directory. Their registry is
workspace-local.

## Preserve Project Invariants

- Reuse the same filesystem-safe `project_id` across reruns.
- Preserve separate `literature/local/` and `literature/pubmed/` ranked outputs.
- Treat immutable run snapshots and provenance as the audit record.
- Do not edit the SQLite literature registry or vector job rows manually.
- Do not claim that file creation proves vector ingestion or citation validity.
- Prefer targeted recovery by literature ID before workspace-wide repair.

## Report Results

After a workflow, report:

- the tools called and why they were chained;
- the workspace and project path;
- query, filters, sort order, and returned record count;
- key files and provenance written;
- citation-validation and query-degradation warnings;
- vector-write setting, collection, status counts, and completeness;
- pending review IDs, fetch errors, or recommended recovery steps.

Point to saved artifacts instead of pasting large records into chat.
