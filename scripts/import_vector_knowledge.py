#!/usr/bin/env python3
"""Import local knowledge documents into a Qdrant vector collection."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import re
import sys
import uuid
from dataclasses import dataclass
from datetime import UTC
from datetime import datetime
from pathlib import Path
from typing import Any
from urllib.error import HTTPError
from urllib.error import URLError
from urllib.parse import quote
from urllib.request import Request
from urllib.request import urlopen


REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_CONFIG_PATH = REPO_ROOT / "configs" / "vector-knowledge.example.json"
DEFAULT_QDRANT_URL = "http://127.0.0.1:6333"
DEFAULT_COLLECTION = "medical_knowledge"
DEFAULT_DISTANCE = "Cosine"
DEFAULT_MAX_CHARS = 2000
DEFAULT_OVERLAP_CHARS = 200
DEFAULT_BATCH_SIZE = 32
DEFAULT_EXTENSIONS = {".md", ".markdown", ".txt", ".jsonl", ".json", ".csv", ".tsv"}
DEFAULT_PAYLOAD_INDEXES = [
    "category",
    "source_type",
    "document_id",
    "chunk_id",
    "project_id",
    "visibility",
    "tags",
]


@dataclass(frozen=True)
class SourceConfig:
    id: str
    path: Path
    glob: str | None
    category: str
    source_type: str
    project_id: str | None
    visibility: str | None
    tags: list[str]
    id_field: str | None
    title_field: str | None
    text_fields: list[str]
    metadata_fields: list[str]


@dataclass(frozen=True)
class ImportConfig:
    qdrant_url: str
    collection: str
    qdrant_api_key_env: str | None
    distance: str
    create_collection: bool
    create_payload_indexes: bool
    embedding_url: str
    embedding_model: str | None
    embedding_api_key_env: str | None
    max_chars: int
    overlap_chars: int
    batch_size: int
    sources: list[SourceConfig]


@dataclass(frozen=True)
class Document:
    document_id: str
    title: str
    text: str
    category: str
    source_type: str
    source_uri: str
    source_id: str
    project_id: str | None
    visibility: str | None
    tags: list[str]
    metadata: dict[str, Any]


@dataclass(frozen=True)
class Chunk:
    point_id: str
    document_id: str
    chunk_id: str
    chunk_index: int
    text: str
    payload: dict[str, Any]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Import local documents into Qdrant for codex-med search_vector_knowledge. "
            "The script reads JSON config by default and has no third-party dependencies."
        )
    )
    parser.add_argument(
        "paths",
        nargs="*",
        type=Path,
        help=(
            "Optional files or directories to import without editing the config. "
            "When omitted, sources are loaded from --config."
        ),
    )
    parser.add_argument(
        "--config",
        type=Path,
        default=DEFAULT_CONFIG_PATH,
        help=f"JSON config path. Defaults to {DEFAULT_CONFIG_PATH}.",
    )
    parser.add_argument("--qdrant-url", default=None)
    parser.add_argument("--collection", default=None)
    parser.add_argument("--embedding-url", default=None)
    parser.add_argument("--embedding-model", default=None)
    parser.add_argument("--category", default=None)
    parser.add_argument("--source-type", default=None)
    parser.add_argument("--project-id", default=None)
    parser.add_argument("--visibility", default=None)
    parser.add_argument("--tag", action="append", default=[])
    parser.add_argument("--glob", default=None)
    parser.add_argument("--id-field", default=None)
    parser.add_argument("--title-field", default=None)
    parser.add_argument("--text-field", action="append", default=[])
    parser.add_argument("--metadata-field", action="append", default=[])
    parser.add_argument("--max-chars", type=int, default=None)
    parser.add_argument("--overlap-chars", type=int, default=None)
    parser.add_argument("--batch-size", type=int, default=None)
    parser.add_argument(
        "--create-collection",
        action="store_true",
        help="Create the Qdrant collection when missing, using the first embedding dimension.",
    )
    parser.add_argument(
        "--create-payload-indexes",
        action="store_true",
        help="Create keyword payload indexes for common filter fields.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Parse and chunk inputs without calling embedding or Qdrant.",
    )
    parser.add_argument(
        "--limit-documents",
        type=int,
        default=None,
        help="Stop after collecting this many documents. Useful for smoke tests.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        config = load_import_config(args)
        documents = collect_documents(config.sources, args.limit_documents)
        chunks = build_chunks(documents, config.max_chars, config.overlap_chars)
        if args.dry_run:
            print_dry_run(config, documents, chunks)
            return 0
        if not chunks:
            print("No chunks collected; nothing to import.")
            return 0
        import_chunks(config, chunks)
        print(
            "imported_chunks={} imported_documents={} collection={}".format(
                len(chunks), len(documents), config.collection
            )
        )
        return 0
    except ImportError as err:
        print(f"error: {err}", file=sys.stderr)
        return 1


class ImportError(RuntimeError):
    pass


def load_import_config(args: argparse.Namespace) -> ImportConfig:
    raw = load_json_config(args.config)
    qdrant = raw.get("qdrant", {})
    embedding = raw.get("embedding", {})
    chunking = raw.get("chunking", {})

    qdrant_url = first_non_empty(
        args.qdrant_url,
        env_value("CODEX_MED_VECTOR_QDRANT_URL"),
        qdrant.get("url"),
        DEFAULT_QDRANT_URL,
    )
    collection = first_non_empty(
        args.collection,
        env_value("CODEX_MED_VECTOR_COLLECTION"),
        qdrant.get("collection"),
        DEFAULT_COLLECTION,
    )
    embedding_url = first_non_empty(
        args.embedding_url,
        env_value("CODEX_MED_EMBEDDING_URL"),
        embedding.get("url"),
    )
    if not embedding_url:
        raise ImportError(
            "embedding URL is required; set CODEX_MED_EMBEDDING_URL, pass --embedding-url, "
            "or define embedding.url in the config"
        )
    embedding_model = first_non_empty(
        args.embedding_model,
        env_value("CODEX_MED_EMBEDDING_MODEL"),
        embedding.get("model"),
    )

    max_chars = positive_int(
        first_non_empty(args.max_chars, chunking.get("max_chars"), DEFAULT_MAX_CHARS),
        "max_chars",
    )
    overlap_chars = non_negative_int(
        first_non_empty(args.overlap_chars, chunking.get("overlap_chars"), DEFAULT_OVERLAP_CHARS),
        "overlap_chars",
    )
    if overlap_chars >= max_chars:
        raise ImportError("overlap_chars must be smaller than max_chars")

    batch_size = positive_int(
        first_non_empty(args.batch_size, raw.get("batch_size"), DEFAULT_BATCH_SIZE),
        "batch_size",
    )

    return ImportConfig(
        qdrant_url=strip_trailing_slash(qdrant_url),
        collection=validate_collection_name(collection),
        qdrant_api_key_env=first_non_empty(
            qdrant.get("api_key_env"),
            "CODEX_MED_VECTOR_QDRANT_API_KEY",
        ),
        distance=first_non_empty(qdrant.get("distance"), DEFAULT_DISTANCE),
        create_collection=bool(qdrant.get("create_collection", False)) or args.create_collection,
        create_payload_indexes=bool(qdrant.get("create_payload_indexes", False))
        or args.create_payload_indexes,
        embedding_url=embedding_url,
        embedding_model=embedding_model,
        embedding_api_key_env=first_non_empty(
            embedding.get("api_key_env"),
            "CODEX_MED_EMBEDDING_API_KEY",
        ),
        max_chars=max_chars,
        overlap_chars=overlap_chars,
        batch_size=batch_size,
        sources=load_sources(raw, args),
    )


def load_json_config(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    if path.suffix.lower() in {".yaml", ".yml"}:
        raise ImportError(
            "YAML config is not supported by this zero-dependency importer; use JSON instead"
        )
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as err:
        raise ImportError(f"failed to parse JSON config {path}: {err}") from err
    if not isinstance(raw, dict):
        raise ImportError("config root must be a JSON object")
    return raw


def load_sources(raw: dict[str, Any], args: argparse.Namespace) -> list[SourceConfig]:
    defaults = raw.get("defaults", {})
    if not isinstance(defaults, dict):
        raise ImportError("defaults must be an object")

    if args.paths:
        return [
            source_from_raw(
                {
                    "id": f"cli_{idx + 1}",
                    "path": str(path),
                    "glob": args.glob,
                    "category": args.category,
                    "source_type": args.source_type,
                    "project_id": args.project_id,
                    "visibility": args.visibility,
                    "tags": args.tag,
                    "id_field": args.id_field,
                    "title_field": args.title_field,
                    "text_fields": args.text_field,
                    "metadata_fields": args.metadata_field,
                },
                {},
            )
            for idx, path in enumerate(args.paths)
        ]

    raw_sources = raw.get("sources", [])
    if not isinstance(raw_sources, list) or not raw_sources:
        raise ImportError("no sources configured; pass paths or add sources to the config")
    return [source_from_raw(source, defaults) for source in raw_sources]


def source_from_raw(raw: dict[str, Any], defaults: dict[str, Any]) -> SourceConfig:
    if not isinstance(raw, dict):
        raise ImportError("each source must be an object")
    path = raw.get("path")
    if not path:
        raise ImportError("source.path is required")

    source_id = first_non_empty(raw.get("id"), Path(path).stem or "source")
    category = first_non_empty(raw.get("category"), defaults.get("category"), "web_knowledge")
    source_type = first_non_empty(raw.get("source_type"), infer_source_type(Path(path)))
    tags = merge_tags(defaults.get("tags", []), raw.get("tags", []))
    text_fields = string_list(raw.get("text_fields", []), "text_fields")
    metadata_fields = string_list(raw.get("metadata_fields", []), "metadata_fields")
    return SourceConfig(
        id=slugify(source_id),
        path=Path(path).expanduser(),
        glob=first_non_empty(raw.get("glob")),
        category=category,
        source_type=source_type,
        project_id=first_non_empty(raw.get("project_id"), defaults.get("project_id")),
        visibility=first_non_empty(raw.get("visibility"), defaults.get("visibility")),
        tags=tags,
        id_field=first_non_empty(raw.get("id_field")),
        title_field=first_non_empty(raw.get("title_field")),
        text_fields=text_fields,
        metadata_fields=metadata_fields,
    )


def collect_documents(sources: list[SourceConfig], limit: int | None) -> list[Document]:
    documents: list[Document] = []
    for source in sources:
        source_documents = documents_from_source(source)
        for document in source_documents:
            if not document.text.strip():
                continue
            documents.append(document)
            if limit is not None and len(documents) >= limit:
                return documents
    return documents


def documents_from_source(source: SourceConfig) -> list[Document]:
    paths = list(iter_source_paths(source))
    documents: list[Document] = []
    for path in paths:
        suffix = path.suffix.lower()
        if suffix == ".jsonl":
            documents.extend(documents_from_jsonl(path, source))
        elif suffix == ".json":
            documents.extend(documents_from_json(path, source))
        elif suffix in {".csv", ".tsv"}:
            documents.extend(documents_from_table(path, source))
        else:
            documents.append(document_from_text_file(path, source))
    return documents


def iter_source_paths(source: SourceConfig) -> list[Path]:
    path = source.path.resolve()
    if path.is_file():
        return [path]
    if not path.exists():
        raise ImportError(f"source path does not exist: {path}")
    glob_pattern = source.glob or "**/*"
    paths = [
        candidate
        for candidate in sorted(path.glob(glob_pattern))
        if candidate.is_file() and candidate.suffix.lower() in DEFAULT_EXTENSIONS
    ]
    return paths


def document_from_text_file(path: Path, source: SourceConfig) -> Document:
    raw = path.read_bytes()
    text = raw.decode("utf-8", errors="replace")
    title = extract_title(text) or path.stem
    metadata = {
        "source_path": str(path),
        "source_relative_path": relative_to_source(path, source.path),
        "content_sha256": hashlib.sha256(raw).hexdigest(),
        "byte_length": len(raw),
        "line_count": text.count("\n") + (1 if text else 0),
    }
    document_id = build_document_id(source, path.stem, metadata["source_relative_path"])
    return Document(
        document_id=document_id,
        title=title,
        text=text,
        category=source.category,
        source_type=source.source_type,
        source_uri=path.as_uri(),
        source_id=source.id,
        project_id=source.project_id,
        visibility=source.visibility,
        tags=source.tags,
        metadata=metadata,
    )


def documents_from_jsonl(path: Path, source: SourceConfig) -> list[Document]:
    documents = []
    for line_no, line in enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as err:
            raise ImportError(f"failed to parse {path}:{line_no} as JSON: {err}") from err
        if not isinstance(row, dict):
            raise ImportError(f"{path}:{line_no} must be a JSON object")
        documents.append(document_from_record(row, source, path, str(line_no)))
    return documents


def documents_from_json(path: Path, source: SourceConfig) -> list[Document]:
    try:
        raw = json.loads(path.read_text(encoding="utf-8", errors="replace"))
    except json.JSONDecodeError as err:
        raise ImportError(f"failed to parse JSON file {path}: {err}") from err
    if isinstance(raw, list):
        return [
            document_from_record(item, source, path, str(idx + 1))
            for idx, item in enumerate(raw)
            if isinstance(item, dict)
        ]
    if isinstance(raw, dict):
        return [document_from_record(raw, source, path, "1")]
    raise ImportError(f"{path} must contain a JSON object or array of objects")


def documents_from_table(path: Path, source: SourceConfig) -> list[Document]:
    delimiter = "\t" if path.suffix.lower() == ".tsv" else ","
    with path.open("r", encoding="utf-8", errors="replace", newline="") as handle:
        reader = csv.DictReader(handle, delimiter=delimiter)
        return [
            document_from_record(row, source, path, str(idx + 1))
            for idx, row in enumerate(reader)
        ]


def document_from_record(
    row: dict[str, Any],
    source: SourceConfig,
    path: Path,
    fallback_id: str,
) -> Document:
    document_key = string_value(row.get(source.id_field)) if source.id_field else fallback_id
    title = (
        string_value(row.get(source.title_field))
        if source.title_field
        else string_value(row.get("title")) or f"{path.stem}:{fallback_id}"
    )
    text = record_text(row, source.text_fields)
    metadata = record_metadata(row, source.metadata_fields)
    metadata.update(
        {
            "source_path": str(path),
            "source_relative_path": relative_to_source(path, source.path),
            "row_id": document_key,
        }
    )
    document_id = build_document_id(source, document_key, str(path))
    return Document(
        document_id=document_id,
        title=title,
        text=text,
        category=source.category,
        source_type=source.source_type,
        source_uri=path.as_uri(),
        source_id=source.id,
        project_id=source.project_id,
        visibility=source.visibility,
        tags=source.tags,
        metadata=metadata,
    )


def record_text(row: dict[str, Any], text_fields: list[str]) -> str:
    fields = text_fields or [key for key in row if row.get(key) not in (None, "")]
    parts = []
    for field in fields:
        value = row.get(field)
        if value in (None, ""):
            continue
        parts.append(f"{field}: {string_value(value)}")
    return "\n".join(parts)


def record_metadata(row: dict[str, Any], metadata_fields: list[str]) -> dict[str, Any]:
    if not metadata_fields:
        return {}
    return {
        field: row[field]
        for field in metadata_fields
        if field in row and row[field] not in (None, "")
    }


def build_chunks(
    documents: list[Document],
    max_chars: int,
    overlap_chars: int,
) -> list[Chunk]:
    chunks: list[Chunk] = []
    imported_at = datetime.now(UTC).isoformat()
    for document in documents:
        for idx, text in enumerate(chunk_text(document.text, max_chars, overlap_chars)):
            chunk_id = f"{document.document_id}:{idx:04d}"
            point_id = str(uuid.uuid5(uuid.NAMESPACE_URL, chunk_id))
            snippet = single_line(text[:1200])
            payload = {
                "document_id": document.document_id,
                "chunk_id": chunk_id,
                "chunk_index": idx,
                "category": document.category,
                "source_type": document.source_type,
                "source_id": document.source_id,
                "source_uri": document.source_uri,
                "title": document.title,
                "snippet": snippet,
                "text": text,
                "tags": document.tags,
                "is_deleted": False,
                "imported_at": imported_at,
            }
            if document.project_id:
                payload["project_id"] = document.project_id
            if document.visibility:
                payload["visibility"] = document.visibility
            payload.update(document.metadata)
            chunks.append(
                Chunk(
                    point_id=point_id,
                    document_id=document.document_id,
                    chunk_id=chunk_id,
                    chunk_index=idx,
                    text=text,
                    payload=payload,
                )
            )
    return chunks


def chunk_text(text: str, max_chars: int, overlap_chars: int) -> list[str]:
    text = normalize_text(text)
    if not text:
        return []
    chunks: list[str] = []
    start = 0
    while start < len(text):
        end = min(start + max_chars, len(text))
        if end < len(text):
            boundary = max(
                text.rfind("\n\n", start, end),
                text.rfind("\n", start, end),
                text.rfind(". ", start, end),
                text.rfind("。", start, end),
            )
            if boundary > start + max_chars // 2:
                end = boundary + 1
        chunk = text[start:end].strip()
        if chunk:
            chunks.append(chunk)
        if end >= len(text):
            break
        start = max(0, end - overlap_chars)
    return chunks


def import_chunks(config: ImportConfig, chunks: list[Chunk]) -> None:
    first_vector = embed_text(config, chunks[0].text)
    if config.create_collection:
        ensure_collection(config, len(first_vector))
    if config.create_payload_indexes:
        ensure_payload_indexes(config)

    pending_points = [
        {
            "id": chunks[0].point_id,
            "vector": first_vector,
            "payload": chunks[0].payload,
        }
    ]
    imported = 0
    for chunk in chunks[1:]:
        pending_points.append(
            {
                "id": chunk.point_id,
                "vector": embed_text(config, chunk.text),
                "payload": chunk.payload,
            }
        )
        if len(pending_points) >= config.batch_size:
            upsert_points(config, pending_points)
            imported += len(pending_points)
            print(f"upserted_chunks={imported}")
            pending_points = []
    if pending_points:
        upsert_points(config, pending_points)
        imported += len(pending_points)
        print(f"upserted_chunks={imported}")


def embed_text(config: ImportConfig, text: str) -> list[float]:
    body: dict[str, Any] = {"input": text}
    if config.embedding_model:
        body["model"] = config.embedding_model
    headers = {"Content-Type": "application/json"}
    api_key = env_value(config.embedding_api_key_env) if config.embedding_api_key_env else None
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    value = http_json("POST", config.embedding_url, body, headers)
    embedding = parse_embedding(value)
    if not embedding:
        raise ImportError("embedding endpoint returned an empty vector")
    return embedding


def parse_embedding(value: Any) -> list[float]:
    if isinstance(value, dict) and isinstance(value.get("embedding"), list):
        raw = value["embedding"]
    elif (
        isinstance(value, dict)
        and isinstance(value.get("data"), list)
        and value["data"]
        and isinstance(value["data"][0], dict)
        and isinstance(value["data"][0].get("embedding"), list)
    ):
        raw = value["data"][0]["embedding"]
    elif isinstance(value, list):
        raw = value
    else:
        raise ImportError(
            "embedding response must contain embedding, data[0].embedding, or a raw array"
        )
    embedding = []
    for item in raw:
        if not isinstance(item, int | float):
            raise ImportError("embedding values must be numbers")
        embedding.append(float(item))
    return embedding


def ensure_collection(config: ImportConfig, vector_size: int) -> None:
    collection_url = qdrant_url(config, f"/collections/{quote(config.collection)}")
    status, _ = http_json_allow_status("GET", collection_url, None, qdrant_headers(config), {404})
    if status != 404:
        return
    body = {
        "vectors": {
            "size": vector_size,
            "distance": config.distance,
        }
    }
    http_json("PUT", collection_url, body, qdrant_headers(config))
    print(f"created_collection={config.collection} vector_size={vector_size}")


def ensure_payload_indexes(config: ImportConfig) -> None:
    url = qdrant_url(config, f"/collections/{quote(config.collection)}/index")
    headers = qdrant_headers(config)
    for field in DEFAULT_PAYLOAD_INDEXES:
        try:
            http_json("PUT", url, {"field_name": field, "field_schema": "keyword"}, headers)
        except ImportError as err:
            print(f"warning: failed to create payload index {field}: {err}", file=sys.stderr)


def upsert_points(config: ImportConfig, points: list[dict[str, Any]]) -> None:
    url = qdrant_url(config, f"/collections/{quote(config.collection)}/points?wait=true")
    http_json("PUT", url, {"points": points}, qdrant_headers(config))


def qdrant_headers(config: ImportConfig) -> dict[str, str]:
    headers = {"Content-Type": "application/json"}
    api_key = env_value(config.qdrant_api_key_env) if config.qdrant_api_key_env else None
    if api_key:
        headers["api-key"] = api_key
    return headers


def qdrant_url(config: ImportConfig, path: str) -> str:
    return f"{config.qdrant_url}{path}"


def http_json(
    method: str,
    url: str,
    body: dict[str, Any] | None,
    headers: dict[str, str],
) -> Any:
    status, value = http_json_allow_status(method, url, body, headers, set())
    if status < 200 or status >= 300:
        raise ImportError(f"{method} {url} returned HTTP {status}: {value}")
    return value


def http_json_allow_status(
    method: str,
    url: str,
    body: dict[str, Any] | None,
    headers: dict[str, str],
    allowed_statuses: set[int],
) -> tuple[int, Any]:
    data = None if body is None else json.dumps(body).encode("utf-8")
    request = Request(url, data=data, headers=headers, method=method)
    try:
        with urlopen(request, timeout=60) as response:
            response_body = response.read().decode("utf-8", errors="replace")
            return response.status, json.loads(response_body) if response_body else {}
    except HTTPError as err:
        response_body = err.read().decode("utf-8", errors="replace")
        if err.code in allowed_statuses:
            return err.code, response_body
        raise ImportError(f"{method} {url} returned HTTP {err.code}: {response_body}") from err
    except URLError as err:
        raise ImportError(f"{method} {url} failed: {err}") from err
    except json.JSONDecodeError as err:
        raise ImportError(f"{method} {url} returned invalid JSON: {err}") from err


def print_dry_run(config: ImportConfig, documents: list[Document], chunks: list[Chunk]) -> None:
    print(
        json.dumps(
            {
                "mode": "dry_run",
                "collection": config.collection,
                "qdrant_url": config.qdrant_url,
                "embedding_url": config.embedding_url,
                "documents": len(documents),
                "chunks": len(chunks),
                "sample_chunks": [
                    {
                        "point_id": chunk.point_id,
                        "document_id": chunk.document_id,
                        "chunk_id": chunk.chunk_id,
                        "category": chunk.payload.get("category"),
                        "source_type": chunk.payload.get("source_type"),
                        "title": chunk.payload.get("title"),
                        "tags": chunk.payload.get("tags"),
                        "source_uri": chunk.payload.get("source_uri"),
                        "text_chars": len(chunk.text),
                    }
                    for chunk in chunks[:5]
                ],
            },
            ensure_ascii=False,
            indent=2,
        )
    )


def extract_title(text: str) -> str | None:
    fallback = None
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        if line.startswith("#"):
            title = line.lstrip("#").strip()
            if title:
                return title[:200]
        elif fallback is None and not is_generic_ocr_header(line):
            fallback = line[:120]
    return fallback


def is_generic_ocr_header(line: str) -> bool:
    normalized = re.sub(r"[^A-Za-z]+", "", line).upper()
    return normalized in {"OPEN", "ARTICLE", "RESEARCHARTICLE", "ORIGINALARTICLE"}


def build_document_id(source: SourceConfig, key: str, stable_basis: str) -> str:
    digest = hashlib.sha1(stable_basis.encode("utf-8")).hexdigest()[:10]
    return f"{source.category}:{source.source_type}:{slugify(key)}:{digest}"


def relative_to_source(path: Path, source_path: Path) -> str:
    root = source_path if source_path.is_dir() else source_path.parent
    try:
        return str(path.relative_to(root.resolve()))
    except ValueError:
        return str(path)


def infer_source_type(path: Path) -> str:
    suffix = path.suffix.lower().lstrip(".")
    return suffix or "filesystem"


def string_value(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value.strip()
    return json.dumps(value, ensure_ascii=False, sort_keys=True)


def normalize_text(text: str) -> str:
    return re.sub(r"\n{3,}", "\n\n", text.replace("\r\n", "\n").replace("\r", "\n")).strip()


def single_line(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()


def slugify(value: str) -> str:
    value = value.strip()
    value = re.sub(r"[^A-Za-z0-9_.-]+", "-", value)
    return value.strip("-") or "item"


def merge_tags(*items: Any) -> list[str]:
    tags = []
    seen = set()
    for item in items:
        for tag in string_list(item, "tags"):
            if tag not in seen:
                seen.add(tag)
                tags.append(tag)
    return tags


def string_list(value: Any, field_name: str) -> list[str]:
    if value in (None, ""):
        return []
    if isinstance(value, str):
        return [value]
    if isinstance(value, list):
        result = []
        for item in value:
            if not isinstance(item, str):
                raise ImportError(f"{field_name} values must be strings")
            if item.strip():
                result.append(item.strip())
        return result
    raise ImportError(f"{field_name} must be a string or list of strings")


def first_non_empty(*values: Any) -> Any:
    for value in values:
        if value is None:
            continue
        if isinstance(value, str):
            value = value.strip()
            if value:
                return value
            continue
        return value
    return None


def env_value(name: str | None) -> str | None:
    if not name:
        return None
    return first_non_empty(os.environ.get(name))


def strip_trailing_slash(value: str) -> str:
    return value.rstrip("/")


def validate_collection_name(value: str) -> str:
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", value):
        raise ImportError(
            "collection may only contain ASCII letters, digits, underscore, hyphen, or dot"
        )
    return value


def positive_int(value: Any, name: str) -> int:
    try:
        parsed = int(value)
    except (TypeError, ValueError) as err:
        raise ImportError(f"{name} must be an integer") from err
    if parsed <= 0:
        raise ImportError(f"{name} must be positive")
    return parsed


def non_negative_int(value: Any, name: str) -> int:
    try:
        parsed = int(value)
    except (TypeError, ValueError) as err:
        raise ImportError(f"{name} must be an integer") from err
    if parsed < 0:
        raise ImportError(f"{name} must be non-negative")
    return parsed


if __name__ == "__main__":
    raise SystemExit(main())
