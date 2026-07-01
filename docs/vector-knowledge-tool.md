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

如果使用已部署的 Qwen3-Embedding-4B / Qwen3-Reranker-4B 和 Qdrant 组成正式 RAG 知识库，见 [Qwen 向量知识库部署与接入文档](qwen-vector-database-deployment.md)。

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

Qdrant token 会作为 `api-key` header 发送给 Qdrant。

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
mkdir -p /data2/wysi/qdrant_storage
docker run -d \
  --name codex-med-qdrant \
  --restart unless-stopped \
  -p 127.0.0.1:6333:6333 \
  -v /data2/wysi/qdrant_storage:/qdrant/storage \
  qdrant/qdrant:latest
```

检查状态：

```bash
curl http://127.0.0.1:6333/collections
```

生产环境建议固定镜像版本，不使用浮动 `latest`。如果需要云服务器部署，继续保持 `6333` 只对内网或 localhost 暴露，外部访问通过 SSH tunnel、反向代理鉴权或内网安全组控制。

## 本地 embedding 服务

仓库提供了一个零依赖的本地哈希 embedding 服务：

```text
scripts/local_hash_embedding_service.py
```

它用于本地联调、端到端导入和 Codex tool 冒烟测试。它是确定性的词法哈希向量，不是生产级语义 embedding。要获得更好的医学语义检索效果，应替换为 BGE、E5、MedCPT、BioBERT/SapBERT embedding 等模型服务，并用同一模型重新导入 collection。

启动示例：

```bash
setsid python scripts/local_hash_embedding_service.py \
  --host 127.0.0.1 \
  --port 18100 \
  --dimensions 512 \
  > /data2/wysi/vector_knowledge_embedding.log 2>&1 < /dev/null &
echo $! > /data2/wysi/vector_knowledge_embedding.pid
```

检查状态：

```bash
curl http://127.0.0.1:18100/healthz
```

停止服务：

```bash
kill "$(cat /data2/wysi/vector_knowledge_embedding.pid)"
```

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

## 导入脚本

仓库内置导入脚本：

```text
scripts/import_vector_knowledge.py
```

脚本支持：

- 读取 Markdown、TXT、JSON、JSONL、CSV、TSV。
- 按分类生成 chunk。
- 调用 embedding endpoint。
- 自动创建 Qdrant collection。
- 创建常用 payload 索引。
- 写入 `document_id`、`chunk_id`、`category`、`source_type`、`source_uri`、`title`、`snippet`、`text`、`tags`、`project_id`、`is_deleted` 等字段。

先 dry-run：

```bash
python scripts/import_vector_knowledge.py \
  --embedding-url http://127.0.0.1:18100/embed \
  --category bio_literature \
  --source-type pubmed_ocr_markdown \
  --project-id pubmed-ocr \
  --tag pubmed \
  --tag ocr \
  --tag markdown \
  --glob '**/*.md' \
  --max-chars 2000 \
  --overlap-chars 200 \
  --dry-run \
  /data1/wysi/pubmed/ocr
```

正式导入：

```bash
python scripts/import_vector_knowledge.py \
  --embedding-url http://127.0.0.1:18100/embed \
  --qdrant-url http://127.0.0.1:6333 \
  --collection medical_knowledge \
  --create-collection \
  --create-payload-indexes \
  --category bio_literature \
  --source-type pubmed_ocr_markdown \
  --project-id pubmed-ocr \
  --tag pubmed \
  --tag ocr \
  --tag markdown \
  --glob '**/*.md' \
  --max-chars 2000 \
  --overlap-chars 200 \
  --batch-size 16 \
  /data1/wysi/pubmed/ocr
```

本次 `/data1/wysi/pubmed/ocr` 导入结果：

```text
documents: 9
chunks: 437
collection: medical_knowledge
category: bio_literature
source_type: pubmed_ocr_markdown
project_id: pubmed-ocr
tags: pubmed, ocr, markdown
```

命令行直接传路径时，source 元数据只使用命令行参数，不会继承 `configs/vector-knowledge.example.json` 里的 `defaults`。配置文件里的 `defaults` 只影响配置文件中的 `sources`。

## 配置文件导入

也可以编辑：

```text
configs/vector-knowledge.example.json
```

然后运行：

```bash
python scripts/import_vector_knowledge.py \
  --config configs/vector-knowledge.example.json
```

配置文件适合维护固定数据源，例如实验记录目录、文献目录、数据库 CSV 导出等。临时导入某个目录时，推荐直接用命令行参数，避免误继承示例默认标签。

## 验证导入

检查 collection：

```bash
curl http://127.0.0.1:6333/collections
```

检查 point 数量：

```bash
curl -H 'Content-Type: application/json' \
  -d '{"exact":true}' \
  http://127.0.0.1:6333/collections/medical_knowledge/points/count
```

本次导入应返回：

```json
{"result":{"count":437},"status":"ok"}
```

启动 Codex 前设置：

```bash
export CODEX_MED_EMBEDDING_URL="http://127.0.0.1:18100/embed"
export CODEX_MED_VECTOR_QDRANT_URL="http://127.0.0.1:6333"
export CODEX_MED_VECTOR_COLLECTION="medical_knowledge"
codex-med
```

进入 Codex 后可以这样问：

```text
请用 search_vector_knowledge 检索 bio_literature 里关于 heavy metal contamination 和 cadmium ecological risk 的内容，返回 5 条来源。
```

运行时检索仍然只由内置 tool 完成。
