#!/usr/bin/env python3
"""Small local embedding HTTP service for vector-knowledge smoke tests.

This is a deterministic lexical hashing embedder. It is useful for local
deployment tests when no neural embedding service is available. For production
semantic retrieval, replace it with a biomedical embedding model and re-import
the collection.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
from http.server import BaseHTTPRequestHandler
from http.server import ThreadingHTTPServer
from typing import Any


TOKEN_RE = re.compile(r"[A-Za-z0-9_+\-.]+|[\u4e00-\u9fff]")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run a deterministic local hash embedding service.")
    parser.add_argument("--host", default=os.environ.get("HASH_EMBEDDING_HOST", "127.0.0.1"))
    parser.add_argument("--port", type=int, default=int(os.environ.get("HASH_EMBEDDING_PORT", "18100")))
    parser.add_argument(
        "--dimensions",
        type=int,
        default=int(os.environ.get("HASH_EMBEDDING_DIMENSIONS", "512")),
        help="Embedding vector size. Must match the Qdrant collection dimension.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.dimensions <= 0:
        raise SystemExit("--dimensions must be positive")
    handler = make_handler(args.dimensions)
    server = ThreadingHTTPServer((args.host, args.port), handler)
    print(f"hash_embedding_service=http://{args.host}:{args.port}/embed dimensions={args.dimensions}")
    server.serve_forever()
    return 0


def make_handler(dimensions: int) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        server_version = "codex-med-hash-embedding/0.1"

        def do_GET(self) -> None:
            if self.path == "/healthz":
                self.respond_json({"ok": True, "dimensions": dimensions})
                return
            self.send_error(404, "not found")

        def do_POST(self) -> None:
            if self.path != "/embed":
                self.send_error(404, "not found")
                return
            try:
                body = self.read_json()
                text = body.get("input")
                if isinstance(text, list):
                    embeddings = [hash_embedding(str(item), dimensions) for item in text]
                    self.respond_json({"data": [{"embedding": embedding} for embedding in embeddings]})
                    return
                if not isinstance(text, str) or not text.strip():
                    self.respond_json({"error": "input must be a non-empty string"}, status=400)
                    return
                self.respond_json({"embedding": hash_embedding(text, dimensions)})
            except Exception as err:  # noqa: BLE001 - HTTP boundary should return JSON errors.
                self.respond_json({"error": str(err)}, status=500)

        def log_message(self, format: str, *args: Any) -> None:
            return

        def read_json(self) -> dict[str, Any]:
            length = int(self.headers.get("content-length", "0"))
            data = self.rfile.read(length)
            value = json.loads(data.decode("utf-8"))
            if not isinstance(value, dict):
                raise ValueError("request body must be a JSON object")
            return value

        def respond_json(self, value: Any, status: int = 200) -> None:
            encoded = json.dumps(value, ensure_ascii=True).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

    return Handler


def hash_embedding(text: str, dimensions: int) -> list[float]:
    vector = [0.0] * dimensions
    tokens = tokenize(text)
    for token in tokens:
        digest = hashlib.sha256(token.encode("utf-8")).digest()
        index = int.from_bytes(digest[:8], "big") % dimensions
        sign = 1.0 if digest[8] & 1 else -1.0
        vector[index] += sign
    norm = math.sqrt(sum(value * value for value in vector))
    if norm == 0:
        return vector
    return [value / norm for value in vector]


def tokenize(text: str) -> list[str]:
    lowered = text.lower()
    tokens = TOKEN_RE.findall(lowered)
    grams: list[str] = []
    for token in tokens:
        grams.append(token)
        if len(token) > 4:
            for size in (3, 4):
                grams.extend(token[idx : idx + size] for idx in range(len(token) - size + 1))
    return grams


if __name__ == "__main__":
    raise SystemExit(main())
