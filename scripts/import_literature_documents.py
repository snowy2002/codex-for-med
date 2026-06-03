#!/usr/bin/env python3
"""Import patent/paper markdown files into the project SQLite knowledge DB."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sqlite3
from dataclasses import dataclass
from datetime import UTC
from datetime import datetime
from pathlib import Path
from typing import Iterable


REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_DB_PATH = REPO_ROOT / "training_ready_v1.sqlite"
PATENT_ID_RE = re.compile(r"^(?:EP|WO|US)\d{4,}[A-Z]?\d?$", re.IGNORECASE)


@dataclass(frozen=True)
class LiteratureDocument:
    document_id: str
    document_type: str
    source_collection: str
    source_path: Path
    source_relative_path: str
    variant: str
    title: str | None
    content_md: str
    content_sha256: str
    byte_length: int
    line_count: int
    metadata_json: str


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Import markdown literature files into literature_documents in "
            "training_ready_v1.sqlite. Re-running updates rows by source_path."
        )
    )
    parser.add_argument(
        "paths",
        nargs="+",
        type=Path,
        help="Markdown files or directories to import recursively.",
    )
    parser.add_argument(
        "--db",
        type=Path,
        default=DEFAULT_DB_PATH,
        help=f"SQLite DB path. Defaults to {DEFAULT_DB_PATH}.",
    )
    parser.add_argument(
        "--document-type",
        choices=("auto", "patent", "paper", "unknown"),
        default="auto",
        help="Document type to store. Use auto to infer patent ids from paths.",
    )
    parser.add_argument(
        "--source-collection",
        default=None,
        help="Collection label. Defaults to the input directory name.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print files that would be imported without writing the database.",
    )
    parser.add_argument(
        "--skip-vlm",
        action="store_true",
        help="Skip markdown files under a vlm directory.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    documents = list(collect_documents(args))
    if args.dry_run:
        for document in documents:
            print(
                f"{document.document_type}\t{document.document_id}\t"
                f"{document.variant}\t{document.source_path}"
            )
        print(f"would import {len(documents)} markdown documents")
        return 0

    args.db.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(args.db) as conn:
        conn.execute("PRAGMA journal_mode=WAL")
        conn.execute("PRAGMA foreign_keys=ON")
        ensure_schema(conn)
        imported = upsert_documents(conn, documents)
        rebuild_fts(conn)
    print(f"imported_or_updated={imported} database={args.db}")
    return 0


def collect_documents(args: argparse.Namespace) -> Iterable[LiteratureDocument]:
    seen: set[Path] = set()
    for input_path in args.paths:
        path = input_path.resolve()
        collection = args.source_collection or infer_source_collection(path)
        for md_path in iter_markdown_files(path):
            if md_path in seen:
                continue
            seen.add(md_path)
            if args.skip_vlm and "vlm" in md_path.parts:
                continue
            yield read_document(
                md_path,
                document_type_arg=args.document_type,
                source_collection=collection,
                source_root=path if path.is_dir() else path.parent,
            )


def iter_markdown_files(path: Path) -> Iterable[Path]:
    if path.is_file():
        if path.suffix.lower() == ".md":
            yield path
        return
    for md_path in sorted(path.rglob("*.md")):
        if md_path.is_file():
            yield md_path.resolve()


def read_document(
    path: Path,
    *,
    document_type_arg: str,
    source_collection: str,
    source_root: Path,
) -> LiteratureDocument:
    raw = path.read_bytes()
    content = raw.decode("utf-8", errors="replace")
    document_id = infer_document_id(path)
    document_type = infer_document_type(document_id, document_type_arg)
    variant = infer_variant(path)
    relative_path = str(path.relative_to(source_root)) if path.is_relative_to(source_root) else str(path)
    metadata = {
        "source_root": str(source_root),
        "importer": "scripts/import_literature_documents.py",
    }
    return LiteratureDocument(
        document_id=document_id,
        document_type=document_type,
        source_collection=source_collection,
        source_path=path,
        source_relative_path=relative_path,
        variant=variant,
        title=extract_title(content, document_id),
        content_md=content,
        content_sha256=hashlib.sha256(raw).hexdigest(),
        byte_length=len(raw),
        line_count=content.count("\n") + (1 if content else 0),
        metadata_json=json.dumps(metadata, ensure_ascii=True, sort_keys=True),
    )


def infer_source_collection(path: Path) -> str:
    if path.is_file():
        return path.parent.name or "literature"
    return path.name or "literature"


def infer_document_id(path: Path) -> str:
    for part in path.parts:
        if PATENT_ID_RE.match(part):
            return part.upper()
    return path.stem


def infer_document_type(document_id: str, document_type_arg: str) -> str:
    if document_type_arg != "auto":
        return document_type_arg
    if PATENT_ID_RE.match(document_id):
        return "patent"
    return "paper"


def infer_variant(path: Path) -> str:
    if "vlm" in path.parts:
        return "vlm"
    return "ocr"


def extract_title(content: str, document_id: str) -> str | None:
    lines = [line.strip() for line in content.splitlines()]
    for idx, line in enumerate(lines):
        normalized = line.lstrip("#").strip()
        if normalized.startswith("(54)"):
            title = normalized.removeprefix("(54)").strip()
            if title:
                return title
            for following in lines[idx + 1 : idx + 6]:
                following = following.lstrip("#").strip()
                if following:
                    return following
    for line in lines:
        if not line.startswith("#"):
            continue
        title = line.lstrip("#").strip()
        if title and title.upper() != document_id.upper() and not title.startswith("("):
            return title
    return None


def ensure_schema(conn: sqlite3.Connection) -> None:
    conn.executescript(
        """
        CREATE TABLE IF NOT EXISTS literature_documents (
            id INTEGER PRIMARY KEY,
            document_id TEXT NOT NULL,
            document_type TEXT NOT NULL CHECK (
                document_type IN ('patent', 'paper', 'unknown')
            ),
            source_collection TEXT NOT NULL,
            source_path TEXT NOT NULL UNIQUE,
            source_relative_path TEXT NOT NULL,
            variant TEXT NOT NULL DEFAULT 'original',
            title TEXT,
            content_md TEXT NOT NULL,
            content_sha256 TEXT NOT NULL,
            byte_length INTEGER NOT NULL,
            line_count INTEGER NOT NULL,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            imported_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_literature_documents_document_id
            ON literature_documents(document_id);
        CREATE INDEX IF NOT EXISTS idx_literature_documents_document_type
            ON literature_documents(document_type);
        CREATE INDEX IF NOT EXISTS idx_literature_documents_collection
            ON literature_documents(source_collection);
        CREATE INDEX IF NOT EXISTS idx_literature_documents_sha
            ON literature_documents(content_sha256);

        CREATE VIRTUAL TABLE IF NOT EXISTS literature_documents_fts USING fts5(
            document_id,
            title,
            content_md,
            content='literature_documents',
            content_rowid='id'
        );
        """
    )


def upsert_documents(
    conn: sqlite3.Connection, documents: Iterable[LiteratureDocument]
) -> int:
    now = datetime.now(UTC).isoformat(timespec="seconds")
    count = 0
    for document in documents:
        conn.execute(
            """
            INSERT INTO literature_documents (
                document_id,
                document_type,
                source_collection,
                source_path,
                source_relative_path,
                variant,
                title,
                content_md,
                content_sha256,
                byte_length,
                line_count,
                metadata_json,
                imported_at,
                updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(source_path) DO UPDATE SET
                document_id=excluded.document_id,
                document_type=excluded.document_type,
                source_collection=excluded.source_collection,
                source_relative_path=excluded.source_relative_path,
                variant=excluded.variant,
                title=excluded.title,
                content_md=excluded.content_md,
                content_sha256=excluded.content_sha256,
                byte_length=excluded.byte_length,
                line_count=excluded.line_count,
                metadata_json=excluded.metadata_json,
                updated_at=excluded.updated_at
            """,
            (
                document.document_id,
                document.document_type,
                document.source_collection,
                str(document.source_path),
                document.source_relative_path,
                document.variant,
                document.title,
                document.content_md,
                document.content_sha256,
                document.byte_length,
                document.line_count,
                document.metadata_json,
                now,
                now,
            ),
        )
        count += 1
    return count


def rebuild_fts(conn: sqlite3.Connection) -> None:
    conn.execute("INSERT INTO literature_documents_fts(literature_documents_fts) VALUES('rebuild')")


if __name__ == "__main__":
    raise SystemExit(main())
