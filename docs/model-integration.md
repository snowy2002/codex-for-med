# 接入自部署模型（Model Integration Guide）

本文档讲清楚两件事：

1. **如何让 codex agent 调用我们自己部署的模型**（如亲和力预测、结构预测、ADMET 等）。
2. **新同事想接入自己的模型时该做什么** —— 一站式 onboarding。

适用场景：你有一个云端部署的模型，对外暴露 `URL + APIKey` 形式的 HTTP 接口，希望 codex agent 能像调用工具一样调用它。

---

## TL;DR

```
codex agent  →  MCP/HTTP  →  gair-model-gateway  →  HTTP + APIKey  →  你的模型服务
```

接入一个新模型的全部工作量：

1. 你的模型服务实现一个 `POST /predict` 接口（业务方自行决定形态）
2. 在 `gair-model-gateway` 的 `models.yaml` 加 8 行配置
3. 在用户的 `~/.codex/config.toml` 启用 `mcp_servers.gair_models`（一次性）

完成。Agent 立即能用 `predict_<your-model>` 这个工具。

---

## 一、架构原理

### 为什么不用 `model-provider`

Codex 内置的 `model-provider`（如 OpenAI/Anthropic）走的是 **Chat Completion / Responses API** 协议 —— 它期望你的模型能进行多轮对话推理。我们这些"预测模型"是单次 input → output 的特化任务（亲和力分数、结构坐标、属性预测），强行套 chat 协议会非常别扭。

### 为什么选 MCP

**MCP（Model Context Protocol）** 是 Anthropic 主推的 LLM 工具/资源标准化协议，codex 一等公民支持。一个 MCP server 可以暴露：

- **Tools** — agent 可调用的函数（我们需要的就是这个）
- **Resources** — 可读取的数据
- **Prompts** — 可复用的提示词

codex 在 `~/.codex/config.toml` 里配置 MCP server 后，自动发现该 server 暴露的所有 tools，把它们注入到 agent 的工具列表里。Agent 选择调用哪个工具完全由模型自己决定，**与 codex 代码完全解耦**。

Codex 支持的 MCP 传输方式（见 `codex-rs/config/src/mcp_types.rs`）：

| 传输 | 用途 |
|---|---|
| `Stdio` | 本地子进程，适合个人开发工具 |
| `StreamableHttp` | 远程 HTTP 服务，**适合云端部署的模型网关** |

`StreamableHttp` 原生支持 `bearer_token_env_var`（从环境变量读 Token）和自定义 HTTP headers —— 鉴权天生就有，不用自己造轮子。

### 为什么是「网关」而不是「每个模型一个 MCP server」

可以每人写自己的 MCP server，但有几个问题：

- 每个团队都要懂 MCP SDK、写鉴权、部署、监控
- codex 用户的 `config.toml` 会变成几十条 `[mcp_servers.xxx]`
- 模型很多时，每个 server 一个 HTTP 端点，运维成本高

**统一网关方案**：一个 `gair-model-gateway` MCP server，内部把所有模型作为独立 tools 暴露。新增模型只需要改网关的配置文件，不需要新部署。

业务方完全不需要知道 MCP 的存在，**他们只负责实现一个标准的 HTTP predict 接口**。

---

## 二、接入流程：业务方视角

> 你是模型作者，想让你的模型出现在 codex agent 里。

### Step 1：实现一个 `POST /predict` 接口

约定如下（团队规范，可在网关层稍作调整）：

**请求**
```http
POST /predict HTTP/1.1
Host: your-model.gair.internal
Authorization: Bearer <your-api-key>
Content-Type: application/json

{
  "inputs": { ... 你的模型输入 ... }
}
```

**响应**
```json
{
  "outputs": { ... 你的模型输出 ... },
  "metadata": { "model_version": "v1.2", "latency_ms": 87 }
}
```

错误响应：
```json
{ "error": { "code": "INVALID_INPUT", "message": "ligand SMILES failed to parse" } }
```

注意：
- 输入/输出 schema 自由设计，但要求是 JSON
- API Key 推荐 `Authorization: Bearer` 头，不要塞 query string
- 超时建议控制在 60s 以内（大模型推理可以更长，但要告知运维）
- 错误用 HTTP 4xx/5xx 状态码，body 提供详细信息

### Step 2：把你的模型注册到网关

在 `gair-model-gateway` 仓库提一个 PR，在 `models.yaml` 加一段：

```yaml
# models.yaml
models:
  predict_affinity:
    description: |
      Predicts binding affinity (pKi) between a small molecule (SMILES)
      and a protein target (UniProt ID).
    endpoint: https://affinity-predictor.gair.internal/predict
    api_key_env: AFFINITY_PREDICTOR_API_KEY
    timeout_seconds: 30
    input_schema:
      type: object
      required: [ligand_smiles, target_uniprot]
      properties:
        ligand_smiles:
          type: string
          description: SMILES string of the small molecule.
        target_uniprot:
          type: string
          description: UniProt accession (e.g. "P00533").
    output_schema:
      type: object
      properties:
        pKi:
          type: number
        confidence:
          type: number
    examples:
      - input: { ligand_smiles: "CCO", target_uniprot: "P00533" }
        output: { pKi: 4.5, confidence: 0.82 }
```

关键字段：

| 字段 | 必填 | 说明 |
|---|---|---|
| `description` | ✅ | **极其重要** —— Agent 选不选你的工具，完全看 description 是否准确描述了用途和适用场景 |
| `endpoint` | ✅ | 你的模型服务 URL |
| `api_key_env` | ✅ | 环境变量名，网关启动时读取，请求时填入 `Authorization: Bearer ...` |
| `timeout_seconds` | ✅ | 单次请求超时 |
| `input_schema` | ✅ | JSON Schema，**Agent 据此生成参数**，properties 的 description 越清楚越好 |
| `output_schema` | ⚪ | 主要用于文档和后续校验 |
| `examples` | ⚪ | 给 Agent few-shot 用 |

### Step 3：在网关部署环境配置 Secret

把 `AFFINITY_PREDICTOR_API_KEY` 加到网关的部署密钥库（K8s Secret / Vault / etc.）。具体由运维同学完成，模型作者只需要提供 key 的获取方式。

### Step 4：联调验证

```bash
# 网关本地启动后，列出所有工具
curl -X POST http://localhost:3030/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/list","id":1}'

# 应能看到你的工具

# 直接调用工具
curl -X POST http://localhost:3030/mcp \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <user-token>" \
  -d '{
    "jsonrpc":"2.0", "id":2, "method":"tools/call",
    "params":{
      "name":"predict_affinity",
      "arguments":{"ligand_smiles":"CCO","target_uniprot":"P00533"}
    }
  }'
```

PR 合并、网关上线后，所有 codex 用户重启就能用。

---

## 三、用户视角：怎么让 codex 用上这些模型

只需要改 **一次** 配置：

### `~/.codex/config.toml`

```toml
[mcp_servers.gair_models]
url = "https://model-gateway.gair.io/mcp"
bearer_token_env_var = "GAIR_GATEWAY_TOKEN"
# 可选：附加 headers
http_headers = { "X-Client" = "codex-cli" }
```

### 设置鉴权 token

```bash
# 加到 ~/.bashrc / ~/.zshrc
export GAIR_GATEWAY_TOKEN="<在内部平台申请的 token>"
```

### 验证

启动 codex：

```bash
codex-med
```

在交互中检查工具是否被注册：

```
/mcp                        # 列出已连接的 MCP servers
/tools                      # 列出所有可用工具
```

应能看到 `predict_affinity`、`predict_structure` 等。然后让 agent 试试：

> "用 ligand SMILES `CCO` 和 target UniProt `P00533` 预测亲和力。"

Agent 会自动选择 `predict_affinity` 工具发起调用。

---

## 四、网关本体的实现骨架

> 这部分是给维护网关的同学看的，不接入新模型不必关心。

推荐用 **Python + FastMCP**，最快上手：

```bash
pip install fastmcp httpx pyyaml jsonschema
```

`gateway.py`（最小可用版本，~80 行）：

```python
import os
import asyncio
import httpx
import yaml
from fastmcp import FastMCP
from jsonschema import validate, ValidationError

mcp = FastMCP("gair-model-gateway")
with open("models.yaml") as f:
    MODELS = yaml.safe_load(f)["models"]

HTTP_CLIENT = httpx.AsyncClient(timeout=httpx.Timeout(120))

def make_tool(name: str, spec: dict):
    """Dynamically register one MCP tool per model entry in models.yaml."""

    async def call(**kwargs):
        # 1) input validation (fail fast before hitting model service)
        try:
            validate(instance=kwargs, schema=spec["input_schema"])
        except ValidationError as e:
            return {"error": {"code": "INVALID_INPUT", "message": str(e)}}

        # 2) read api key from env
        api_key = os.environ.get(spec["api_key_env"])
        if not api_key:
            return {"error": {"code": "AUTH_MISCONFIG",
                              "message": f"env {spec['api_key_env']} not set"}}

        # 3) forward
        try:
            r = await HTTP_CLIENT.post(
                spec["endpoint"],
                json={"inputs": kwargs},
                headers={"Authorization": f"Bearer {api_key}"},
                timeout=spec.get("timeout_seconds", 60),
            )
            r.raise_for_status()
            data = r.json()
        except httpx.HTTPError as e:
            return {"error": {"code": "UPSTREAM_ERROR", "message": str(e)}}

        return data.get("outputs", data)

    # register with MCP. FastMCP reads docstring + signature for schema; we override
    # by passing tool annotations explicitly.
    mcp.tool(
        name=name,
        description=spec["description"],
        # FastMCP supports passing a JSON Schema directly for inputs:
        input_schema=spec["input_schema"],
    )(call)

for name, spec in MODELS.items():
    make_tool(name, spec)

if __name__ == "__main__":
    # StreamableHttp transport on port 3030
    mcp.run(transport="streamable-http", host="0.0.0.0", port=3030)
```

**部署**：放进一个 Docker 镜像，挂上 K8s Secret 提供各模型的 API key，对外暴露一个 HTTPS endpoint。

**可观测**（强烈建议补齐）：
- 每次工具调用记一条 access log（user_token hash, tool_name, latency, status）
- Prometheus 指标：`gateway_tool_calls_total{tool, status}`、`gateway_tool_latency_seconds`
- 上游模型错误率告警

**鉴权（生产环境）**：
- 入口处校验用户的 `Authorization: Bearer <user-token>` —— 不要让 codex 客户端裸奔
- 维护一份 `<user-token, allowed_tools>` ACL，按用户/团队控制可见工具

---

## 五、设计决策的"为什么"

下面这些坑是踩过/想清楚后的结论，新加入的同学可以省事：

**Q1：为什么不直接在 codex 里加一个 HTTP 工具，让用户在 config.toml 里配 URL？**

A：可行但不可扩展。每个用户要自己写一段 schema、维护 URL 列表、处理鉴权。集中网关后，模型上下线、Schema 变更对用户透明。

**Q2：为什么用 MCP 而不是 OpenAPI / function-calling 的 JSON schema？**

A：MCP 是 codex 一等公民支持的协议，无需改动 codex 代码。用 OpenAPI 需要写一个 OpenAPI→tool 的适配层，等于又造一遍 MCP 轮子。

**Q3：为什么每个模型一个 tool，而不是一个 `predict(model_name, inputs)` 通用 tool？**

A：单一通用 tool 会让 agent 在选择时缺少 schema 提示，**它根本不知道每个 model 的输入格式**。每模型一 tool，input_schema 直接进 LLM 上下文，调用准确率高得多。

**Q4：网关挂了怎么办？**

A：网关本质是无状态反向代理，可以多副本部署 + LB。每个 model 的故障隔离：网关收到 5xx 返回结构化错误，不会让 agent 卡死。

**Q5：模型推理特别慢怎么办（>2 min）？**

A：MCP 支持长轮询/流式。短期推荐：网关端把模型调用 detach 成异步任务，返回 `job_id`，提供配套的 `check_job_status(job_id)` 工具。

**Q6：能不能让模型自带 prompt 模板传给 agent？**

A：可以。MCP 协议也支持 `prompts`，在 `models.yaml` 加 `prompt_template` 字段，网关把它注册为 MCP prompt，codex 用户可以 `/prompts` 查看并使用。

---

## 六、Checklist：接入一个新模型时按这个走

- [ ] 模型服务实现 `POST /predict`，约定输入/输出 JSON 结构
- [ ] 拿到 API Key，存到内部 Secret Manager
- [ ] 在 `gair-model-gateway/models.yaml` 提交 PR，填齐 `description` / `input_schema` / `endpoint` / `api_key_env`
- [ ] 本地起网关 `python gateway.py`，用 `curl` 调通 `tools/call`
- [ ] 把 Secret 加到网关的部署环境
- [ ] 通知组内：你的工具 `predict_xxx` 已上线，可在 codex 中使用
- [ ] （可选）写一段 `examples`，提高 agent 调用准确率

---

## 附录：相关代码位置

| 文件 | 作用 |
|---|---|
| `codex-rs/config/src/mcp_types.rs` | MCP server 配置结构定义（`McpServerTransportConfig` 等） |
| `codex-rs/config/src/mcp_edit.rs` | 加载 `~/.codex/config.toml` 里的 mcp_servers |
| `codex-rs/mcp-client/` | codex 自己作为 MCP **客户端**的实现，连接到外部 MCP server |
| `codex-rs/mcp-server/` | codex **作为** MCP server 暴露自己功能的实现（用不到，本接入方案是反过来） |
| `docs/config.md` | 用户配置文档（如需新增配置项，更新此处） |
