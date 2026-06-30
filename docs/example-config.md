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
```

Set the client token in your shell:

```shell
export GAIR_GATEWAY_TOKEN="<token configured in GATEWAY_BEARER_TOKENS>"
```

After restart, Codex should discover gateway tools such as
`predict_dlp_affinity`.
