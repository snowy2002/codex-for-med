---
name: codex-med-science-workbench
description: Choose and use codex-med science workbench tools for biomedical research requests. Use when the user asks to inspect available medical knowledge backends, describe the codex-med database, make a durable literature/evidence map, compare local vector knowledge with PubMed, create research_projects evidence packages, or decide between list_med_knowledge_collections, describe_med_database, literature_map, and pubmed_literature_map.
---

# Codex Med Science Workbench

Use this skill to route biomedical research requests to the right codex-med tool and to keep generated evidence packages reproducible.

## Tool Routing

Start with the user's intended artifact.

- Use `list_med_knowledge_collections` when the user asks what medical knowledge sources are available, what backends exist, or how to choose a retrieval source.
- Use `describe_med_database` when the user needs schema, table purpose, row counts, vector collection metadata, payload fields, categories, or deployed database details.
- Use `literature_map` when the user wants a durable evidence package from the local codex-med vector knowledge base. Prefer it for curated/local knowledge, offline-style package creation, and requests that mention `research_projects/<project_id>` without requiring live PubMed.
- Use `pubmed_literature_map` when the user asks for PubMed, PMID, MeSH, abstracts, recent papers, public literature search, NCBI, or an external literature map. Also use it when the user wants a new PubMed-backed workflow without changing the existing vector-only `literature_map`.

If the user asks for a quick answer or a few citations in chat, do not create a project package unless they ask for a map, workflow, export, report, provenance, or saved files.

## Workflow Order

For unfamiliar biomedical tasks, call tools in this order:

1. `list_med_knowledge_collections` to see available sources.
2. `describe_med_database` if source schemas or collection contents matter.
3. Choose one map workflow:
   - `literature_map` for local vector evidence.
   - `pubmed_literature_map` for live PubMed evidence.

Do not call both map tools by default. Use both only when the user asks to compare local/codex-med evidence against PubMed or when one source is clearly insufficient for the question.

## Parameter Guidance

For both map tools:

- Always provide a clear `topic`.
- Set `project_id` when the user names a project or when rerunning/appending to an existing project.
- Keep IDs filesystem-safe and stable across reruns.

For `literature_map`:

- Use `top_k` for the number of distinct local documents to keep.
- Use `category` only when the user specifies a known vector category or asks to constrain local retrieval.
- Use `year_range` as a report label; it does not perform PubMed-style date filtering.

For `pubmed_literature_map`:

- Use `pubmed_query` when the user supplies Boolean logic, field tags, journal filters, author filters, or an exact PubMed query.
- Use `min_year` and `max_year` together for publication-year filtering.
- Use `sort: "pub_date"` for newest/recent-paper requests; otherwise use relevance.
- Keep `retmax` modest unless the user explicitly asks for broad coverage.
- Leave `fetch_abstracts` enabled unless the user wants fast metadata-only output.

## Expected Outputs

Map workflows should create or update `research_projects/<project_id>/` with standard project structure:

- `literature/` for records, reports, and citations.
- `code/` for downstream analysis scripts or notebooks.
- `analysis/` for derived tables and analysis notes.
- `figures/` for plots and visual outputs.
- `provenance/` for run metadata.
- `project.json` at the project root as the run registry.

Do not overwrite or repurpose the vector-only `literature_map` output contract when using PubMed. The PubMed workflow must remain a separate tool and workflow name.

## Response Style

After using a map workflow, summarize:

- Which tool ran and why.
- The project path and key files written.
- Record count and important filters such as query, years, sort, and abstract fetching.
- Any degraded search behavior or per-record fetch errors.

Do not paste large records into chat when files were created. Point to the saved project artifacts instead.
