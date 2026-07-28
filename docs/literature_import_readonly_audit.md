# Literature import read-only audit

Date: 2026-07-27 (Asia/Shanghai)

Target collection: `medical_knowledge_qwen3_4b`

This audit used only the collection metadata endpoint and Qdrant's read-only
`points/scroll` operation. It issued no point upsert, delete, payload update,
index creation, or snapshot mutation.

## Observed baseline

| Metric | Value |
| --- | ---: |
| Qdrant points | 50,760 |
| Indexed vectors | 50,760 |
| Segments | 8 |
| Distinct `document_id` | 316 |
| Distinct non-empty `paper_id` | 288 |
| Distinct non-empty `source_uri` | 288 |
| Points without `paper_id` | 2,376 |
| Documents without `paper_id` | 28 |
| Existing `project_id` values | `data-extract-new` |

All 28 documents without `paper_id` contain a recognized patent identifier in
their file `source_uri`. The 28 inferred identifiers are unique, and every one
exactly matches a `paper_id` in the 288 complete source documents. Therefore the
baseline initializer can map 316 source documents to 288 canonical literature
records without title-based automatic merging.

The point count remained 50,760 after the audit. Production vector writes are
still not approved; a recoverable Qdrant snapshot and human review of the
initializer dry-run report remain required before setting
`CODEX_MED_PUBMED_VECTOR_WRITES=1`.
