# Qwen 向量知识库部署与接入文档

本文说明如何把 Qwen3 embedding / reranker 模型、Qdrant 向量数据库和 `codex-med` 的 `search_vector_knowledge` 工具串起来。目标是让 Codex 能通过 tool 检索生物文献、Web 知识、数据库真实数据、实验记录等分类知识。

注意：本文不要写入真实 token。真实 token 只放在本机私有环境文件中，例如 `/home/wysi/.codex/codex-med-vector.env`。

## 当前已验证的模型服务

### Embedding

服务地址：

```text
http://gw-bzokqkvr2cblz8ok6y.cn-wulanchabu-acdr-1.pai-eas.aliyuncs.com/api/predict/qwen3_embedding_4b/v1/embeddings
```

模型名：

```text
/model_dir/Qwen3-Embedding-4B
```

接口类型：

```text
OpenAI-compatible embeddings API
```

已验证输出维度：

```text
2560
```

请求示例：

```bash
curl -sS \
  -H "Authorization: Bearer ${CODEX_MED_EMBEDDING_API_KEY}" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "/model_dir/Qwen3-Embedding-4B",
    "input": "antibody affinity and spike protein binding"
  }' \
  "http://gw-bzokqkvr2cblz8ok6y.cn-wulanchabu-acdr-1.pai-eas.aliyuncs.com/api/predict/qwen3_embedding_4b/v1/embeddings"
```

返回格式包含：

```json
{
  "model": "/model_dir/Qwen3-Embedding-4B",
  "data": [
    {
      "embedding": [0.1, 0.2, 0.3]
    }
  ]
}
```

### Reranker

服务地址：

```text
http://gw-bzokqkvr2cblz8ok6y.cn-wulanchabu-acdr-1.pai-eas.aliyuncs.com/api/predict/qwen3_reranker_4b/v1/rerank
```

模型名：

```text
/model_dir/Qwen3-Reranker-4B
```

接口类型：

```text
OpenAI-compatible rerank API
```

请求示例：

```bash
curl -sS \
  -H "Authorization: Bearer ${CODEX_MED_RERANKER_API_KEY}" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "/model_dir/Qwen3-Reranker-4B",
    "query": "antibody affinity and spike protein binding",
    "documents": [
      "This paper studies neutralizing antibodies against SARS-CoV-2 spike protein."
    ]
  }' \
  "http://gw-bzokqkvr2cblz8ok6y.cn-wulanchabu-acdr-1.pai-eas.aliyuncs.com/api/predict/qwen3_reranker_4b/v1/rerank"
```

返回格式包含：

```json
{
  "model": "/model_dir/Qwen3-Reranker-4B",
  "results": [
    {
      "index": 0,
      "relevance_score": 0.4324
    }
  ]
}
```

当前状态：

- `search_vector_knowledge` 已支持通过 embedding endpoint 查询 Qdrant。
- reranker endpoint 已验证可用。
- reranker 排序步骤还没有接入当前 Rust tool 运行链路。正式接入时建议流程为：Qdrant 召回 top 50，再用 reranker 重排，最后返回 top 8 给 Codex。

## 总体架构

当前可用链路：

```text
codex-med
  -> search_vector_knowledge
  -> Qwen3-Embedding-4B /v1/embeddings
  -> Qdrant vector search
  -> 返回 top_k chunks 给 Codex
```

推荐正式链路：

```text
codex-med
  -> search_vector_knowledge
  -> Qwen3-Embedding-4B /v1/embeddings
  -> Qdrant recall top 50
  -> Qwen3-Reranker-4B /v1/rerank
  -> final top 8
  -> 返回给 Codex
```

Qdrant 只负责存储向量和 payload，不负责生成 embedding，也不负责 rerank。

## 私有环境变量配置

建议将真实 token 写入本机私有文件：

```text
/home/wysi/.codex/codex-med-vector.env
```

文件权限：

```bash
chmod 600 /home/wysi/.codex/codex-med-vector.env
```

内容格式：

```bash
export CODEX_MED_EMBEDDING_URL="http://gw-bzokqkvr2cblz8ok6y.cn-wulanchabu-acdr-1.pai-eas.aliyuncs.com/api/predict/qwen3_embedding_4b/v1/embeddings"
export CODEX_MED_EMBEDDING_MODEL="/model_dir/Qwen3-Embedding-4B"
export CODEX_MED_EMBEDDING_API_KEY="<embedding-token>"

export CODEX_MED_RERANKER_URL="http://gw-bzokqkvr2cblz8ok6y.cn-wulanchabu-acdr-1.pai-eas.aliyuncs.com/api/predict/qwen3_reranker_4b/v1/rerank"
export CODEX_MED_RERANKER_MODEL="/model_dir/Qwen3-Reranker-4B"
export CODEX_MED_RERANKER_API_KEY="<reranker-token>"

export CODEX_MED_VECTOR_RECALL_TOP_K="50"
export CODEX_MED_VECTOR_FINAL_TOP_K="8"
```

使用时加载：

```bash
source /home/wysi/.codex/codex-med-vector.env
```

不要把这个文件提交到 git。

## 部署 Qdrant 向量数据库

### 本地部署

```bash
mkdir -p /data2/wysi/qdrant_storage

docker run -d \
  --name codex-med-qdrant \
  --restart unless-stopped \
  -p 127.0.0.1:6333:6333 \
  -v /data2/wysi/qdrant_storage:/qdrant/storage \
  qdrant/qdrant:latest
```

检查：

```bash
curl http://127.0.0.1:6333/collections
```

### 云服务器部署

服务器上建议使用固定目录保存数据：

```bash
sudo mkdir -p /data/qdrant/storage
sudo chown -R "$USER:$USER" /data/qdrant
```

只监听内网或 localhost：

```bash
docker run -d \
  --name codex-med-qdrant \
  --restart unless-stopped \
  -p 127.0.0.1:6333:6333 \
  -v /data/qdrant/storage:/qdrant/storage \
  qdrant/qdrant:latest
```

如果需要从其他机器访问，优先使用 SSH tunnel：

```bash
ssh -L 6333:127.0.0.1:6333 user@server
```

然后本地仍然访问：

```text
http://127.0.0.1:6333
```

不建议把 Qdrant 直接暴露到公网。必须暴露时，应使用内网安全组、Nginx 鉴权、HTTPS 和 Qdrant API key。

## Collection 设计

当前旧测试库使用的是本地 hash embedding，维度是 512，collection 名为：

```text
medical_knowledge
```

Qwen3-Embedding-4B 输出维度是 2560，不能写入旧的 512 维 collection。建议新建：

```text
medical_knowledge_qwen3_4b
```

向量配置：

```json
{
  "vectors": {
    "size": 2560,
    "distance": "Cosine"
  }
}
```

手动创建 collection：

```bash
curl -X PUT \
  -H "Content-Type: application/json" \
  -d '{"vectors":{"size":2560,"distance":"Cosine"}}' \
  http://127.0.0.1:6333/collections/medical_knowledge_qwen3_4b
```

通常不需要手动创建，`scripts/import_vector_knowledge.py --create-collection` 会根据第一条 embedding 的维度自动创建。

## Payload 字段规范

每个 chunk 写入 Qdrant 时至少应包含：

```json
{
  "document_id": "bio_literature:pubmed_ocr_markdown:example:abc123",
  "chunk_id": "bio_literature:pubmed_ocr_markdown:example:abc123:0001",
  "chunk_index": 1,
  "category": "bio_literature",
  "source_type": "pubmed_ocr_markdown",
  "source_uri": "file:///data1/wysi/pubmed/ocr/example.md",
  "title": "Example Paper Title",
  "snippet": "short text shown to Codex",
  "text": "full chunk text",
  "tags": ["pubmed", "ocr", "markdown"],
  "project_id": "pubmed-ocr",
  "is_deleted": false
}
```

推荐分类：

| category | 用途 |
| --- | --- |
| `bio_literature` | 生物医学文献、论文 OCR、PubMed 文本 |
| `web_knowledge` | Web 页面、产品文档、公开知识 |
| `database_record` | 真实数据库记录、CSV/TSV 导出 |
| `experiment_record` | 实验记录、实验日志、实验结论 |
| `protocol` | 实验方案、SOP、操作步骤 |
| `clinical_guideline` | 临床指南 |
| `patent` | 专利 |
| `internal_note` | 内部笔记 |

建议建立 payload 索引：

```text
category
source_type
document_id
chunk_id
project_id
visibility
tags
```

导入脚本加上 `--create-payload-indexes` 会自动创建这些索引。

## 上传数据到向量数据库

### 导入 Markdown 文献目录

示例：导入 `/data1/wysi/pubmed/ocr` 中的 Markdown OCR 文档。

先加载模型配置：

```bash
cd /data2/wysi/codex-for-med
source /home/wysi/.codex/codex-med-vector.env

export CODEX_MED_VECTOR_QDRANT_URL="http://127.0.0.1:6333"
export CODEX_MED_VECTOR_COLLECTION="medical_knowledge_qwen3_4b"
```

先 dry-run，确认文档数、chunk 数、标题和标签：

```bash
python scripts/import_vector_knowledge.py \
  --qdrant-url "$CODEX_MED_VECTOR_QDRANT_URL" \
  --collection "$CODEX_MED_VECTOR_COLLECTION" \
  --embedding-url "$CODEX_MED_EMBEDDING_URL" \
  --embedding-model "$CODEX_MED_EMBEDDING_MODEL" \
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
  --qdrant-url "$CODEX_MED_VECTOR_QDRANT_URL" \
  --collection "$CODEX_MED_VECTOR_COLLECTION" \
  --embedding-url "$CODEX_MED_EMBEDDING_URL" \
  --embedding-model "$CODEX_MED_EMBEDDING_MODEL" \
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

检查数量：

```bash
curl -H "Content-Type: application/json" \
  -d '{"exact":true}' \
  "http://127.0.0.1:6333/collections/medical_knowledge_qwen3_4b/points/count"
```

### 导入实验记录

```bash
python scripts/import_vector_knowledge.py \
  --qdrant-url "$CODEX_MED_VECTOR_QDRANT_URL" \
  --collection "$CODEX_MED_VECTOR_COLLECTION" \
  --embedding-url "$CODEX_MED_EMBEDDING_URL" \
  --embedding-model "$CODEX_MED_EMBEDDING_MODEL" \
  --create-collection \
  --create-payload-indexes \
  --category experiment_record \
  --source-type markdown \
  --project-id dlp-affinity \
  --tag experiment \
  --tag dlp-affinity \
  --glob '**/*.md' \
  /path/to/experiment-notes
```

### 导入数据库 CSV

CSV 适合一行一条记录。通过 `--id-field`、`--title-field`、`--text-field`、`--metadata-field` 指定字段。

```bash
python scripts/import_vector_knowledge.py \
  --qdrant-url "$CODEX_MED_VECTOR_QDRANT_URL" \
  --collection "$CODEX_MED_VECTOR_COLLECTION" \
  --embedding-url "$CODEX_MED_EMBEDDING_URL" \
  --embedding-model "$CODEX_MED_EMBEDDING_MODEL" \
  --create-collection \
  --create-payload-indexes \
  --category database_record \
  --source-type csv \
  --project-id lab-db \
  --id-field sample_id \
  --title-field sample_name \
  --text-field sample_name \
  --text-field target_protein \
  --text-field assay_summary \
  --text-field conclusion \
  --metadata-field sample_type \
  --metadata-field created_at \
  /path/to/lab_samples.csv
```

### 导入 JSONL

JSONL 适合来自 Web crawler、文献解析或 ETL 的结构化文本。

```json
{"id":"doc-001","title":"Example title","text":"Long content...","url":"https://example.com/a","source":"web"}
```

导入：

```bash
python scripts/import_vector_knowledge.py \
  --qdrant-url "$CODEX_MED_VECTOR_QDRANT_URL" \
  --collection "$CODEX_MED_VECTOR_COLLECTION" \
  --embedding-url "$CODEX_MED_EMBEDDING_URL" \
  --embedding-model "$CODEX_MED_EMBEDDING_MODEL" \
  --create-collection \
  --create-payload-indexes \
  --category web_knowledge \
  --source-type jsonl \
  --project-id web-corpus \
  --id-field id \
  --title-field title \
  --text-field title \
  --text-field text \
  --metadata-field url \
  --metadata-field source \
  /path/to/web_knowledge.jsonl
```

## Codex 如何接入向量数据库

启动 `codex-med` 前加载环境变量：

```bash
source /home/wysi/.codex/codex-med-vector.env

export CODEX_MED_VECTOR_QDRANT_URL="http://127.0.0.1:6333"
export CODEX_MED_VECTOR_COLLECTION="medical_knowledge_qwen3_4b"

codex-med
```

进入 Codex 后检查工具：

```text
/tools
```

应看到：

```text
search_vector_knowledge
```

可以直接要求 Codex：

```text
请用 search_vector_knowledge 在 bio_literature 中检索 cadmium ecological risk watershed，返回 5 条来源和摘要。
```

等价工具参数：

```json
{
  "query": "cadmium ecological risk watershed soil heavy metal contamination",
  "categories": ["bio_literature"],
  "filters": {
    "project_id": "pubmed-ocr"
  },
  "top_k": 5,
  "collection": "medical_knowledge_qwen3_4b"
}
```

当前 tool 会：

1. 调用 `CODEX_MED_EMBEDDING_URL` 把 query 转成 2560 维向量。
2. 调用 Qdrant collection 做向量检索。
3. 返回 score、title、snippet、source_uri、category、payload 等信息给 Codex。

## Reranker 接入方案

当前 reranker 服务已验证，但 `search_vector_knowledge` 还没有使用它。建议后续实现如下：

1. 新增环境变量：

```bash
export CODEX_MED_RERANKER_URL="http://gw-bzokqkvr2cblz8ok6y.cn-wulanchabu-acdr-1.pai-eas.aliyuncs.com/api/predict/qwen3_reranker_4b/v1/rerank"
export CODEX_MED_RERANKER_MODEL="/model_dir/Qwen3-Reranker-4B"
export CODEX_MED_RERANKER_API_KEY="<reranker-token>"
export CODEX_MED_VECTOR_RECALL_TOP_K="50"
export CODEX_MED_VECTOR_FINAL_TOP_K="8"
```

2. 修改 tool 查询逻辑：

```text
query
  -> embedding
  -> Qdrant recall top 50
  -> collect payload.text or snippet
  -> reranker query + documents
  -> sort by relevance_score
  -> return final top 8
```

3. Reranker 请求体：

```json
{
  "model": "/model_dir/Qwen3-Reranker-4B",
  "query": "用户问题",
  "documents": [
    "chunk text 1",
    "chunk text 2"
  ]
}
```

4. 返回结果中建议增加：

```json
{
  "score": 0.72,
  "rerank_score": 0.91
}
```

注意：reranker 不参与入库。只有 embedding 参与入库和 query vector 生成。

## 迁移到服务器

### 方案一：重新导入

这是最稳妥的迁移方式。

1. 在服务器部署 Qdrant。
2. 在服务器配置 `/etc/codex-med/vector.env` 或 `/home/<user>/.codex/codex-med-vector.env`。
3. 把原始 Markdown、CSV、JSONL 数据同步到服务器。
4. 在服务器重新运行导入脚本。

优点：

- collection 维度和模型一定一致。
- 可以顺便修正 chunk 参数和 metadata。
- 不依赖 Qdrant 底层存储兼容性。

同步数据：

```bash
rsync -av /data1/wysi/pubmed/ocr user@server:/data/codex-med-data/pubmed/ocr
```

服务器导入：

```bash
cd /data2/wysi/codex-for-med
source /home/wysi/.codex/codex-med-vector.env
export CODEX_MED_VECTOR_QDRANT_URL="http://127.0.0.1:6333"
export CODEX_MED_VECTOR_COLLECTION="medical_knowledge_qwen3_4b"

python scripts/import_vector_knowledge.py \
  --qdrant-url "$CODEX_MED_VECTOR_QDRANT_URL" \
  --collection "$CODEX_MED_VECTOR_COLLECTION" \
  --embedding-url "$CODEX_MED_EMBEDDING_URL" \
  --embedding-model "$CODEX_MED_EMBEDDING_MODEL" \
  --create-collection \
  --create-payload-indexes \
  --category bio_literature \
  --source-type pubmed_ocr_markdown \
  --project-id pubmed-ocr \
  --tag pubmed \
  --tag ocr \
  --tag markdown \
  --glob '**/*.md' \
  /data/codex-med-data/pubmed/ocr
```

### 方案二：Qdrant snapshot

适合数据量大、不想重新 embedding 的场景。

创建 snapshot：

```bash
curl -X POST \
  "http://127.0.0.1:6333/collections/medical_knowledge_qwen3_4b/snapshots"
```

查看 snapshot：

```bash
curl \
  "http://127.0.0.1:6333/collections/medical_knowledge_qwen3_4b/snapshots"
```

下载 snapshot：

```bash
curl -o medical_knowledge_qwen3_4b.snapshot \
  "http://127.0.0.1:6333/collections/medical_knowledge_qwen3_4b/snapshots/<snapshot-name>"
```

上传到服务器：

```bash
scp medical_knowledge_qwen3_4b.snapshot user@server:/data/qdrant-snapshots/
```

恢复 snapshot：

```bash
curl -X POST \
  -F "snapshot=@/data/qdrant-snapshots/medical_knowledge_qwen3_4b.snapshot" \
  "http://127.0.0.1:6333/collections/medical_knowledge_qwen3_4b/snapshots/upload"
```

### 方案三：停止 Qdrant 后 rsync 存储目录

只在同版本 Qdrant 且可以停机时使用。

```bash
docker stop codex-med-qdrant
rsync -av /data2/wysi/qdrant_storage/ user@server:/data/qdrant/storage/
docker start codex-med-qdrant
```

优先级建议：

```text
重新导入 > snapshot > rsync storage
```

## 服务器上的系统化启动

可以把环境变量放到：

```text
/etc/codex-med/vector.env
```

权限：

```bash
sudo chmod 600 /etc/codex-med/vector.env
```

启动 Codex：

```bash
set -a
source /etc/codex-med/vector.env
set +a

export CODEX_MED_VECTOR_QDRANT_URL="http://127.0.0.1:6333"
export CODEX_MED_VECTOR_COLLECTION="medical_knowledge_qwen3_4b"

codex-med
```

如果需要让多人使用，建议：

- 每个用户有自己的 Codex 登录态。
- Qdrant 和模型服务走内网。
- token 文件只给运行用户读。
- 不在 shell history 中粘贴 token。

## 更新、删除和重导数据

### 更新

用相同路径、相同 source_type、相同 category 和相同 chunk 参数重新运行导入脚本，会使用稳定 `point_id` 覆盖已有 point。

适合：

- 修正文档内容。
- 更新 OCR 文本。
- 补充 metadata。

### 删除

推荐软删除：

```json
{
  "is_deleted": true
}
```

当前 `search_vector_knowledge` 默认会排除 `is_deleted=true` 的 point。

如果要物理删除，可以通过 Qdrant filter 删除，例如按 `project_id`：

```bash
curl -X POST \
  -H "Content-Type: application/json" \
  -d '{
    "filter": {
      "must": [
        {"key": "project_id", "match": {"value": "pubmed-ocr"}}
      ]
    }
  }' \
  "http://127.0.0.1:6333/collections/medical_knowledge_qwen3_4b/points/delete?wait=true"
```

### 改 chunk 参数后的重导

如果改变了 `--max-chars`、`--overlap-chars` 或 splitter 逻辑，旧 chunk 的 point_id 可能不再完全覆盖。建议先按 `project_id` 删除旧数据，再重导。

## 常见问题

### 直接 POST 到 `/api/predict/qwen3_embedding_4b` 返回 404

正确路径需要带 OpenAI-compatible 子路由：

```text
/v1/embeddings
```

完整路径：

```text
http://gw-bzokqkvr2cblz8ok6y.cn-wulanchabu-acdr-1.pai-eas.aliyuncs.com/api/predict/qwen3_embedding_4b/v1/embeddings
```

reranker 同理，需要：

```text
/v1/rerank
```

### Qdrant 报 vector dimension mismatch

原因通常是把 2560 维 Qwen embedding 写入了旧的 512 维 hash collection。

解决：

```text
使用新 collection：medical_knowledge_qwen3_4b
```

或者删除并重建旧 collection。

### Codex 检索没有结果

检查：

```bash
source /home/wysi/.codex/codex-med-vector.env
echo "$CODEX_MED_EMBEDDING_URL"
echo "$CODEX_MED_EMBEDDING_MODEL"
echo "$CODEX_MED_VECTOR_QDRANT_URL"
echo "$CODEX_MED_VECTOR_COLLECTION"
curl http://127.0.0.1:6333/collections
```

再检查 collection 计数：

```bash
curl -H "Content-Type: application/json" \
  -d '{"exact":true}' \
  "http://127.0.0.1:6333/collections/${CODEX_MED_VECTOR_COLLECTION}/points/count"
```

### token 不能写进文档吗

不能。token 属于密钥，只能放在：

```text
/home/wysi/.codex/codex-med-vector.env
/etc/codex-med/vector.env
云服务器 Secret Manager
CI/CD secret
```

不要放进：

```text
docs/
configs/*.json
README.md
git commit
shell history
```

## 推荐落地顺序

1. 部署 Qdrant。
2. 写好私有 env 文件。
3. 验证 Qwen embedding `/v1/embeddings`。
4. 新建或自动创建 `medical_knowledge_qwen3_4b`。
5. dry-run 导入数据。
6. 正式导入数据。
7. 启动 `codex-med` 并设置 collection。
8. 用 `search_vector_knowledge` 测试检索。
9. 后续把 reranker 接入 tool，提升排序质量。
