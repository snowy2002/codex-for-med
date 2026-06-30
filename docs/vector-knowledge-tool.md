# 内置向量知识库工具

本文说明如何让 `codex-med` 通过内置 tool 检索向量知识库。这个方案不需要部署 MCP 网关，也不需要部署单独的知识库 gateway。

## 架构

```text
codex-med
  -> built-in tool: search_vector_knowledge
  -> embedding endpoint
  -> Qdrant vector database
```

Codex 只看到一个内置工具：

```text
search_vector_knowledge
```

它负责：

- 把自然语言问题发送给 embedding endpoint，得到 query embedding。
- 直接调用 Qdrant collection 做向量检索。
- 支持按 `category`、`project_id`、`tags` 等 payload 字段过滤。
- 返回带来源、分类、分数、片段和 citation 的 JSON。

## 为什么不走网关

模型预测和知识检索是两类不同能力：

- DLP-Affinity 这类模型预测仍然可以走模型网关。
- 向量知识库检索现在作为 `codex-med` 的内置 tool 直接接入。

因此向量知识库不需要加入 `codex-for-med-gateway`，也不需要在 Codex 配置 `[mcp_servers.med_knowledge]`。

## 后端要求

需要两个后端：

1. Embedding endpoint
2. Qdrant

Embedding endpoint 可以是本地服务、内网服务或兼容 OpenAI embeddings 响应格式的服务。

工具发送请求：

```http
POST <CODEX_MED_EMBEDDING_URL>
Content-Type: application/json
Authorization: Bearer <CODEX_MED_EMBEDDING_API_KEY>

{
  "input": "用户查询",
  "model": "可选模型名"
}
```

支持三种响应格式：

```json
{
  "embedding": [0.1, 0.2, 0.3]
}
```

```json
{
  "data": [
    {
      "embedding": [0.1, 0.2, 0.3]
    }
  ]
}
```

```json
[0.1, 0.2, 0.3]
```

Qdrant 使用 REST API：

```text
POST /collections/{collection}/points/search
```

## 环境变量

最小配置：

```bash
export CODEX_MED_EMBEDDING_URL="http://127.0.0.1:18100/embed"
export CODEX_MED_VECTOR_QDRANT_URL="http://127.0.0.1:6333"
export CODEX_MED_VECTOR_COLLECTION="medical_knowledge"
```

可选配置：

```bash
export CODEX_MED_EMBEDDING_MODEL="bge-m3"
export CODEX_MED_EMBEDDING_API_KEY="<embedding-token>"
export CODEX_MED_VECTOR_QDRANT_API_KEY="<qdrant-token>"
```

也兼容这些通用变量：

```bash
export EMBEDDING_API_KEY="<embedding-token>"
export QDRANT_API_KEY="<qdrant-token>"
```

## Qdrant payload 约定

推荐每个 point 的 payload 至少包含：

```json
{
  "document_id": "bio_literature:pmid:12345678",
  "chunk_id": "bio_literature:pmid:12345678:0001",
  "category": "bio_literature",
  "title": "文档标题",
  "snippet": "可直接返回给 Codex 的文本片段",
  "source_uri": "https://pubmed.ncbi.nlm.nih.gov/12345678/",
  "citation": "PMID:12345678",
  "project_id": "dlp-affinity",
  "tags": ["antibody", "affinity"],
  "is_deleted": false
}
```

推荐分类：

| category | 含义 |
| --- | --- |
| `bio_literature` | 生物文献 |
| `web_knowledge` | Web 知识 |
| `database_record` | 数据库真实数据 |
| `experiment_record` | 实验记录 |
| `protocol` | 实验方案 |
| `clinical_guideline` | 临床指南 |
| `patent` | 专利 |
| `internal_note` | 内部笔记 |

工具默认会排除 `is_deleted=true` 的 point。没有 `is_deleted` 字段的 point 不会被这个默认过滤排除。

## Tool 参数

`search_vector_knowledge` 参数：

| 参数 | 必填 | 说明 |
| --- | --- | --- |
| `query` | 是 | 自然语言查询 |
| `categories` | 否 | 分类数组，例如 `["bio_literature", "experiment_record"]` |
| `filters` | 否 | payload 精确过滤，例如 `{"project_id": "dlp-affinity"}` |
| `top_k` | 否 | 返回数量，默认 8，最大 30 |
| `collection` | 否 | 覆盖默认 Qdrant collection |
| `qdrant_url` | 否 | 覆盖默认 Qdrant 地址 |
| `embedding_url` | 否 | 覆盖默认 embedding endpoint |
| `embedding_model` | 否 | 覆盖默认 embedding model |
| `include_payload` | 否 | 是否返回 payload，默认 true |

## 调用示例

Codex 中可以直接要求：

```text
请用 search_vector_knowledge 查询 DLP-Affinity 相关实验记录，返回最相关的 5 条来源和摘要。
```

工具调用等价参数：

```json
{
  "query": "DLP-Affinity 抗体抗原亲和力预测实验记录",
  "categories": ["experiment_record"],
  "filters": {
    "project_id": "dlp-affinity"
  },
  "top_k": 5
}
```

跨分类检索：

```json
{
  "query": "抗体亲和力预测常用评价指标",
  "categories": ["bio_literature", "experiment_record", "database_record"],
  "filters": {
    "tags": ["antibody", "affinity"]
  },
  "top_k": 8
}
```

## 返回示例

```json
{
  "query": "抗体亲和力预测常用评价指标",
  "backend": {
    "provider": "qdrant",
    "url": "http://127.0.0.1:6333/",
    "collection": "medical_knowledge"
  },
  "returned_results": 1,
  "results": [
    {
      "rank": 1,
      "score": 0.91,
      "point_id": "bio_literature:pmid:12345678:0001",
      "document_id": "bio_literature:pmid:12345678",
      "chunk_id": "bio_literature:pmid:12345678:0001",
      "category": "bio_literature",
      "title": "Example paper title",
      "snippet": "相关文本片段...",
      "source": "https://pubmed.ncbi.nlm.nih.gov/12345678/",
      "citation": "PMID:12345678"
    }
  ]
}
```

## 本地 Qdrant 启动示例

```bash
docker run --rm \
  -p 6333:6333 \
  -v /data2/wysi/qdrant_storage:/qdrant/storage \
  qdrant/qdrant
```

生产环境建议固定镜像版本，不使用浮动 `latest`。

## 和 Codex 配置的关系

这个工具是内置工具，不需要在 `~/.codex/config.toml` 增加 MCP 配置。

只需要确保启动 `codex-med` 的 shell 里有必要环境变量：

```bash
export CODEX_MED_EMBEDDING_URL="http://127.0.0.1:18100/embed"
export CODEX_MED_VECTOR_QDRANT_URL="http://127.0.0.1:6333"
export CODEX_MED_VECTOR_COLLECTION="medical_knowledge"
codex-med
```

进入 Codex 后用 `/tools` 检查是否存在：

```text
search_vector_knowledge
```

## 数据写入

`search_vector_knowledge` 是只读工具。文档切分、embedding、upsert 到 Qdrant 应通过离线脚本完成。

推荐写入流程：

```text
文档/数据库/实验记录
  -> parser
  -> chunker
  -> embedding
  -> Qdrant upsert
```

这样可以避免 Codex 运行时误写、误删或污染知识库。

## 后续开发建议

下一步可以在本仓库增加导入脚本：

```text
scripts/import_vector_knowledge.py
```

脚本职责：

- 读取 Markdown、PDF、网页快照、数据库导出、实验记录。
- 按分类生成 chunk。
- 调用同一个 embedding endpoint。
- 写入 Qdrant collection。
- 强制写入 `document_id`、`chunk_id`、`category`、`source_uri`、`citation`、`tags`、`project_id`。

运行时检索仍然只由内置 tool 完成。
