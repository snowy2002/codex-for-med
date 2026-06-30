# 接入自部署模型（Model Integration Guide）

本文说明如何让 `codex-med` 调用自部署医学预测模型，例如
DLP-Affinity、结构预测、ADMET 等。

当前推荐架构：

```text
codex-med -> MCP Streamable HTTP -> codex-for-med-gateway -> HTTP /predict -> 模型服务
```

网关仓库：

```text
git@github.com:snowy2002/codex-for-med-gateway.git
```

## 为什么走 MCP 网关

不要把预测模型接成 Codex `model-provider`。`model-provider` 面向 Chat
Completion / Responses API，对应多轮对话模型；DLP-Affinity 这类模型是单次
`input -> output` 的专业预测工具。

Codex 原生支持 MCP server。只要在 `~/.codex/config.toml` 配置一个
Streamable HTTP MCP server，Codex 会发现网关暴露的 tools，并把它们注入到
agent 可调用工具列表。

统一网关的好处：

- Codex 侧只配置一次 `[mcp_servers.gair_models]`
- 新模型只需要改网关 `models.yaml`
- 模型 API key、审计、超时、错误格式统一在网关处理
- 模型服务不需要理解 MCP，只要实现 HTTP `/predict`

## Codex 配置

本地开发：

```toml
[mcp_servers.gair_models]
url = "http://127.0.0.1:3030/mcp"
bearer_token_env_var = "GAIR_GATEWAY_TOKEN"
startup_timeout_sec = 20
tool_timeout_sec = 180
```

生产环境把 `url` 换成网关 HTTPS 地址即可。

Shell 中设置客户端 token：

```bash
export GAIR_GATEWAY_TOKEN="<token configured in GATEWAY_BEARER_TOKENS>"
```

启动 `codex-med` 后可用：

```text
/mcp
/tools
```

检查是否发现 `predict_dlp_affinity` 等工具。

## 网关运行

```bash
cd /data2/wysi/codex-for-med-gateway
python -m venv .venv
. .venv/bin/activate
pip install -e '.[dev]'

export GATEWAY_BEARER_TOKENS="<token used by codex>"
export DLP_AFFINITY_API_KEY="<token used for DLP-Affinity service>"

codex-for-med-gateway --config models.yaml --host 0.0.0.0 --port 3030
```

健康检查：

```bash
curl http://127.0.0.1:3030/healthz
curl http://127.0.0.1:3030/readyz
```

## 模型服务契约

业务方模型服务只需要实现：

```http
POST /predict
Authorization: Bearer <model-api-key>
Content-Type: application/json

{
  "inputs": {
    "...": "..."
  }
}
```

推荐响应：

```json
{
  "outputs": {
    "...": "..."
  },
  "metadata": {
    "model_version": "optional",
    "latency_ms": 87
  }
}
```

错误响应使用 HTTP 4xx/5xx，并返回：

```json
{
  "error": {
    "code": "INVALID_INPUT",
    "message": "human readable reason"
  }
}
```

## DLP-Affinity 接入

`codex-for-med-gateway/models.yaml` 已内置：

```yaml
models:
  predict_dlp_affinity:
    description: |
      Predict antibody-antigen binding affinity K_D from antibody and antigen
      amino-acid sequences using DLP-Affinity.
    endpoint: http://127.0.0.1:8001/predict
    api_key_env: DLP_AFFINITY_API_KEY
    timeout_seconds: 180
    input_schema:
      type: object
      additionalProperties: false
      required: [seq_ab, seq_ag]
      properties:
        seq_ab:
          type: string
          description: Antibody amino-acid sequence.
        seq_ag:
          type: string
          description: Antigen amino-acid sequence.
```

DLP-Affinity 后台服务请求：

```json
{
  "inputs": {
    "seq_ab": "QVQLVQSG...",
    "seq_ag": "NITNLCPF..."
  }
}
```

DLP-Affinity 后台服务响应：

```json
{
  "outputs": {
    "predicted_kd_log10": -8.1,
    "predicted_kd": 7.9e-9
  }
}
```

注意：DLP-Affinity 是抗体-抗原亲和力预测模型，输入是氨基酸序列，不是
SMILES 小分子配体。

## 新增模型流程

1. 模型服务实现 HTTP `POST /predict`
2. 在网关 `models.yaml` 增加一个 tool 配置
3. 把模型服务 API key 加到网关部署环境变量
4. 本地运行网关并用 Codex `/tools` 确认工具出现
5. 推送并部署网关

`models.yaml` 字段：

| 字段 | 必填 | 说明 |
|---|---|---|
| `description` | 是 | Agent 选择工具主要依赖它，必须写清楚适用场景 |
| `endpoint` | 是 | 模型服务 `/predict` 地址 |
| `api_key_env` | 否 | 网关读取此环境变量，并用 bearer token 调上游 |
| `timeout_seconds` | 是 | 单次模型调用超时 |
| `input_schema` | 是 | JSON Schema，决定 Codex 生成参数的形状 |
| `output_schema` | 否 | 文档和后续校验使用 |

## 生产部署建议

- 网关无状态，可多副本部署
- `GATEWAY_BEARER_TOKENS` 用 Secret 管理，不写入 git
- 每个模型的 API key 独立环境变量，例如 `DLP_AFFINITY_API_KEY`
- 上游模型慢调用建议把 `tool_timeout_sec` 和 `timeout_seconds` 都调到足够大
- 日志中不要记录完整 token 或敏感序列
- 后续如需多团队权限，可在网关中加入 `<token, allowed_tools>` ACL

## 相关代码

| 文件 | 作用 |
|---|---|
| `codex-rs/config/src/mcp_types.rs` | Codex MCP server 配置结构 |
| `codex-rs/config/src/mcp_edit.rs` | 从 `~/.codex/config.toml` 加载 MCP 配置 |
| `codex-rs/core/src/tools/spec_plan.rs` | 把 MCP tools 注册进 Codex 工具路由 |
| `codex-for-med-gateway/models.yaml` | 网关模型 tool 注册表 |
