# Codex 调用网关模型流程

## 总体链路

```mermaid
flowchart LR
    User[用户请求] --> Codex[Codex Agent]
    Codex --> ToolSelect[选择 MCP 工具<br/>predict_dlp_affinity]
    ToolSelect --> Gateway[MCP 网关<br/>codex-for-med-gateway<br/>127.0.0.1:13030/mcp]
    Gateway --> Registry[(模型注册表<br/>models.yaml)]
    Registry --> Spec[读取模型配置<br/>endpoint / input_schema / api_key_env]
    Spec --> Validate[校验输入参数<br/>seq_ab / seq_ag]
    Validate --> Model[DLP-Affinity 模型服务<br/>127.0.0.1:18001/predict]
    Model --> Result[预测结果<br/>predicted_kd_log10 / predicted_kd]
    Result --> Gateway
    Gateway --> Codex
    Codex --> User
```

Codex 不直接调用模型服务，而是先调用 MCP 工具。网关根据 `models.yaml` 找到对应模型配置，校验输入后转发到模型 HTTP `/predict` 接口，再把模型返回结果传回 Codex。

## 请求时序

```mermaid
sequenceDiagram
    actor User as 用户
    participant Codex as Codex Agent
    participant Gateway as codex-for-med-gateway
    participant Config as models.yaml
    participant Model as DLP-Affinity Service

    User->>Codex: 请求预测抗体-抗原亲和力
    Codex->>Gateway: MCP tools/call: predict_dlp_affinity
    Gateway->>Config: 查询 predict_dlp_affinity 配置
    Config-->>Gateway: endpoint / input_schema / api_key_env
    Gateway->>Gateway: 校验 seq_ab / seq_ag
    Gateway->>Model: POST /predict<br/>Authorization: Bearer ${DLP_AFFINITY_API_KEY}
    Model-->>Gateway: outputs: predicted_kd_log10, predicted_kd
    Gateway-->>Codex: MCP ToolResult
    Codex-->>User: 展示模型预测结果
```

## 当前已接入模型

```mermaid
flowchart TB
    Gateway[codex-for-med-gateway] --> DLP[predict_dlp_affinity]
    DLP --> DLPInputs[输入<br/>seq_ab: 抗体氨基酸序列<br/>seq_ag: 抗原氨基酸序列]
    DLP --> DLPOutputs[输出<br/>predicted_kd_log10<br/>predicted_kd]
    DLP --> DLPEndpoint[模型服务<br/>http://127.0.0.1:18001/predict]
```

当前网关注册的工具是 `predict_dlp_affinity`。它用于 DLP-Affinity 抗体-抗原亲和力预测。

## 后续模型接入口

```mermaid
flowchart LR
    Gateway[codex-for-med-gateway] --> Registry[(models.yaml)]

    Registry --> Existing[predict_dlp_affinity<br/>已接入]
    Registry --> FutureA[predict_admet<br/>预留]
    Registry --> FutureB[predict_structure_quality<br/>预留]
    Registry --> FutureC[predict_new_model<br/>预留]

    FutureA --> ServiceA[ADMET /predict]
    FutureB --> ServiceB[Structure Quality /predict]
    FutureC --> ServiceC[任意新模型 /predict]
```

后续接入新模型时，只需要在 `models.yaml` 新增一个 tool block，并让新模型服务实现统一的 `POST /predict` HTTP 接口。网关会把每个 `models.yaml` 里的模型条目注册成一个 Codex 可调用的 MCP 工具。

统一模型服务接口：

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

推荐返回：

```json
{
  "outputs": {
    "...": "..."
  },
  "metadata": {
    "model_version": "optional",
    "latency_ms": 123
  }
}
```
