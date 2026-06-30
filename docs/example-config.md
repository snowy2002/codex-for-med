# Sample configuration

For a sample configuration file, see [this documentation](https://developers.openai.com/codex/config-sample).

## Medical Model Gateway

To expose self-hosted biomedical models as Codex tools, run
`codex-for-med-gateway` and add a Streamable HTTP MCP server:

```toml
[mcp_servers.gair_models]
url = "http://127.0.0.1:3030/mcp"
bearer_token_env_var = "GAIR_GATEWAY_TOKEN"
startup_timeout_sec = 20
tool_timeout_sec = 180
default_tools_approval_mode = "approve"
```

Set the client token in your shell:

```shell
export GAIR_GATEWAY_TOKEN="<token configured in GATEWAY_BEARER_TOKENS>"
```

After restart, Codex should discover gateway tools such as
`predict_dlp_affinity`.

## Built-in Vector Knowledge Tool

The vector knowledge search is a built-in `codex-med` tool, not an MCP server.
No `[mcp_servers]` entry is required.

Set the backend environment variables before starting `codex-med`:

```shell
export CODEX_MED_EMBEDDING_URL="http://127.0.0.1:18100/embed"
export CODEX_MED_VECTOR_QDRANT_URL="http://127.0.0.1:6333"
export CODEX_MED_VECTOR_COLLECTION="medical_knowledge"
```

Optional:

```shell
export CODEX_MED_EMBEDDING_MODEL="bge-m3"
export CODEX_MED_EMBEDDING_API_KEY="<embedding-token>"
export CODEX_MED_VECTOR_QDRANT_API_KEY="<qdrant-token>"
```

After restart, `/tools` should show `search_vector_knowledge`.
