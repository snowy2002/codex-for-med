# 文献登记、双来源项目文献地图与 PubMed 向量入库最终方案

## 1. 文档状态与实施目标

本文是该功能的最终实施方案，替代此前的讨论稿。当前代码、全局文献数据库和线上
Qdrant 尚未全部按本文完成改造。

本次实施需要同时完成以下目标：

1. `literature_map` 只表示从本地 Qdrant 检索到的文献。
2. `pubmed_literature_map` 只表示从线上 PubMed 检索到的文献。
3. 两个 workflow 在同一个研究项目中独立落盘，不互相覆盖。
4. 每篇真实文献拥有一个永久、全局唯一的 `literature_id`。
5. 项目通过 `literature_id` 引用全局文献，不在 ID 文件中复制文献元数据。
6. PubMed 新文献使用现有 Qdrant payload 结构完成 embedding 和入库。
7. Qdrant 不增加新 payload 字段，现有 points 不批量回填或重写。
8. 同一篇文献在整个 Qdrant collection 中默认只保留一套向量。
9. 重复运行、并发运行和失败重试不会生成第二套随机 points。
10. 删除研究项目不影响全局文献元数据或共享向量。
11. 所有正式 collection 写入都必须在临时 collection 验证通过后显式启用。

本文中的“全局”默认指同一个 codex-med workspace。第一版采用单机共享 SQLite；
如果将来需要多主机并发写入，再按相同字段语义迁移到 PostgreSQL。

## 2. 已确定的技术决策

### 2.1 全局文献数据库

第一版使用 SQLite，默认路径为：

```text
<workspace_root>/.codex-med/literatures.sqlite3
```

允许通过以下环境变量覆盖：

```text
CODEX_MED_LITERATURE_DB
```

约束：

- 数据库不得放在任一 `research_projects/<project_id>/` 内。
- 启动连接时设置 `PRAGMA foreign_keys=ON`、`journal_mode=WAL` 和
  `busy_timeout=5000` 毫秒。
- 所有空外部标识在入库前转换为 SQL `NULL`，不能用空字符串绕过唯一约束。
- schema 通过版本化 migration 创建和升级，不允许运行时散落执行临时 DDL。
- SQLite 第一版只承诺同一主机、同一 workspace 的并发安全，不用于多主机共享写入。

### 2.2 全局主键

`literature_id` 使用随机 UUIDv4。系统一旦发出该 ID，就不得改变或分配给另一篇文献；
它不从 point、chunk、文件路径、标题、PMID 或 DOI 计算。

### 2.3 Qdrant collection

正式 collection 仍为：

```text
medical_knowledge_qwen3_4b
```

开发和集成测试必须使用显式传入的临时 collection。代码不得因缺少配置而把测试写入
正式 collection。

### 2.4 当前结果与历史结果

每种 workflow 同时保留：

- 固定路径的“最近一次成功结果”，供用户和后续工具直接使用。
- 以 `run_id` 命名的不可变运行快照，供追溯和恢复使用。

固定路径可以被同一种 workflow 的下一次成功运行更新，但 local 和 PubMed 永远不共用
固定路径。历史快照不得覆盖。

## 3. 系统边界

系统分为三个独立部分：

```text
<workspace_root>/.codex-med/literatures.sqlite3
  保存永久 literature_id、文献元数据、标识冲突和向量任务状态

research_projects/<project_id>/
  保存本项目两种检索来源的 literature_id、报告、引用和运行记录

Qdrant
  保存可召回的文献 chunks、embedding 和现有 payload
```

引用方向为：

```text
项目文件 -> literature_id -> 全局文献元数据
Qdrant paper_id/source_uri -> 标准化强标识 -> 全局文献元数据
向量任务状态 -> literature_id + Qdrant collection
```

`literatures` 主表不保存以下内容：

- 研究项目 `project_id`
- Qdrant `point_id`、`chunk_id` 或 `document_id`
- Qdrant collection
- 项目文件路径
- 项目检索排名或分数
- 从文献元数据反向指向项目或 point 的引用

向量任务状态属于可恢复的运行控制数据，不属于文献元数据。

## 4. 数据库 schema

数据库包含 migration 表、一个文献主表以及四个辅助表。辅助表用于冲突、别名、来源
记录映射和可恢复的向量任务，不是第二份文献元数据。

```sql
CREATE TABLE literature_schema_migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at TEXT NOT NULL
);
```

每个 migration 在一个事务中执行；应用启动时只能向前升级，遇到未知的更高版本必须
停止写入，不能猜测降级。

### 4.1 文献主表

```sql
CREATE TABLE literatures (
    literature_id TEXT PRIMARY KEY,
    pmid TEXT UNIQUE,
    doi TEXT UNIQUE,
    paper_id TEXT UNIQUE,
    title TEXT,
    abstract TEXT,
    authors_json TEXT NOT NULL DEFAULT '[]',
    journal TEXT,
    publication_date TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

一篇真实文献在此表中只能有一行。`authors_json` 和 `metadata_json` 必须始终是合法 JSON。
无法用单个字符串表达的日期精度、出版类型、MeSH、语言、撤稿/勘误关系、原始标识和
字段来源写入 `metadata_json`。

`metadata_json` 第一版采用以下稳定顶层键，缺失集合使用空数组或空对象：

```json
{
  "raw_identifiers": {},
  "field_sources": {},
  "mesh_terms": [],
  "publication_types": [],
  "language": null,
  "relations": [],
  "flags": {
    "abstract_missing": false,
    "retracted": false,
    "corrected": false
  }
}
```

新增扩展键必须向后兼容；不能改变上述键的类型。所有数据库时间使用 UTC RFC 3339，
UUID 字符串统一使用小写规范形式。

### 4.2 人工复核表

```sql
CREATE TABLE literature_review_cases (
    review_case_id TEXT PRIMARY KEY,
    conflict_type TEXT NOT NULL CHECK (
        conflict_type IN ('identifier_conflict', 'possible_duplicate')
    ),
    incoming_identifiers_json TEXT NOT NULL,
    matched_literature_ids_json TEXT NOT NULL,
    incoming_metadata_json TEXT NOT NULL,
    status TEXT NOT NULL CHECK (
        status IN (
            'pending',
            'resolved_same_literature',
            'resolved_different_literatures',
            'ignored_invalid_input'
        )
    ),
    resolution_json TEXT,
    created_at TEXT NOT NULL,
    resolved_at TEXT
);
```

`conflict_type` 只能为 `identifier_conflict` 或 `possible_duplicate`。前者表示强标识互相
矛盾，后者表示标题、作者、年份等弱特征需要人工确认。

`status` 只能为：

```text
pending
resolved_same_literature
resolved_different_literatures
ignored_invalid_input
```

多个强标识分别命中不同 `literature_id` 时：

- 不自动合并。
- 不覆盖任一现有强标识。
- 创建 `pending` 冲突记录。
- 本次项目运行记录冲突状态。
- 默认不为该记录新建向量，等待人工处理。

弱重复候选也写入该表，避免只在某次 provenance 中出现后丢失。它不占用 PMID、DOI 或
`paper_id`，人工确认相同后通过 alias 合并，确认不同后解除向量任务阻塞。

### 4.3 文献 ID 别名表

```sql
CREATE TABLE literature_id_aliases (
    alias_literature_id TEXT PRIMARY KEY,
    canonical_literature_id TEXT NOT NULL
        REFERENCES literatures(literature_id),
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL
);
```

只有人工确认两条历史记录确属同一篇文献时才使用别名：

1. 在一个事务中选定 canonical `literature_id`。
2. 合并非冲突元数据和强标识。
3. 将被合并 ID 写入别名表。
4. 将来源记录和仍需保留的运行控制数据重新绑定到 canonical ID。
5. 删除被合并的主表行。

旧项目中的 ID 仍可通过别名解析到 canonical ID。别名 ID 永远不得重新分配。正常读取
项目 ID 时必须解析别名；项目下一次成功重跑时应把固定路径 ID 文件改写为 canonical
ID，历史快照保持不变。

### 4.4 来源记录映射表

```sql
CREATE TABLE literature_source_records (
    source_system TEXT NOT NULL,
    source_record_key TEXT NOT NULL,
    literature_id TEXT NOT NULL
        REFERENCES literatures(literature_id),
    match_method TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (source_system, source_record_key)
);
```

用途：

- 将同一个 Qdrant `document_id` 的所有 chunks 稳定映射到一个 `literature_id`。
- 将历史重复导入产生的两个 `document_id` 映射到同一个 canonical ID。
- 让缺少 PMID、DOI、可靠 `paper_id` 的本地记录在重跑时仍复用原 ID。
- 记录映射是通过强标识、人工确认还是仅通过同一来源记录建立。

`source_system` 必须包含命名空间，例如 `qdrant:<collection>` 或 `pubmed`；
`source_record_key` 可以是 Qdrant `document_id` 或 PMID，但不能使用 point/chunk ID。
该表不改变“一篇真实文献在 `literatures` 主表中只有一行”的约束。

### 4.5 向量任务状态表

```sql
CREATE TABLE literature_vector_jobs (
    job_id TEXT PRIMARY KEY,
    literature_id TEXT NOT NULL
        REFERENCES literatures(literature_id),
    collection_name TEXT NOT NULL,
    embedding_profile TEXT NOT NULL,
    status TEXT NOT NULL CHECK (
        status IN (
            'pending',
            'embedding',
            'upserting',
            'verifying',
            'complete',
            'already_vectorized',
            'possible_duplicate',
            'blocked_conflict',
            'failed'
        )
    ),
    expected_point_count INTEGER,
    verified_point_count INTEGER,
    existing_dataset TEXT,
    review_case_id TEXT
        REFERENCES literature_review_cases(review_case_id),
    owner_token TEXT,
    lease_expires_at TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (literature_id, collection_name)
);
```

`status` 只能为：

```text
pending
embedding
upserting
verifying
complete
already_vectorized
possible_duplicate
blocked_conflict
failed
```

该表不保存 Qdrant point ID。PubMed point ID 可由稳定的 `document_id` 和 chunk index
重新计算。

任务领取使用 SQLite 事务和带过期时间的 lease：

- 一个进程成功领取后，其他进程不得重复 embedding。
- lease 未过期时，其他进程等待或跳过并在 provenance 中记录。
- lease 过期后允许新进程接管，并先检查 Qdrant 已完成的 points，再只补缺失部分。
- `complete` 或 `already_vectorized` 默认不自动重新 embedding。
- owner token 使用 UUIDv4；lease 默认 15 分钟，持有者至少每 60 秒续租一次。

## 5. 标识标准化、登记与人工合并

### 5.1 标准化函数

初始化、本地检索、PubMed 登记、Qdrant 查重和测试必须调用同一组标准化函数。

PMID：

```text
PMID: 12345678
https://pubmed.ncbi.nlm.nih.gov/12345678/
  -> 12345678
```

规则为去除已知前缀/URL、trim，并验证剩余内容只包含十进制数字。非法值不作为强标识。

DOI：

```text
https://doi.org/10.1000/ABC.123
doi:10.1000/ABC.123
  -> 10.1000/abc.123
```

规则为 URL decode、去除已知前缀、trim、转小写，并去除明确不属于 DOI 的外围空白。
原始 DOI 保存在 `metadata_json.raw_identifiers`，以便审计。不能无条件删除 DOI 内部的
合法标点。

`paper_id`：

- trim 并折叠外围空白。
- `PMID:<数字>` 统一转成 PMID，同时保留规范形式 `PMID:<PMID>`。
- 已识别的专利号去除空格和常见分隔符后转大写，例如
  `ep 0323806 a1 -> EP0323806A1`。
- 未识别类型只做保守标准化，不能把不同命名空间的普通 ID 强行合并。

`source_uri`：

- 从已知 PubMed URL、DOI URL、专利页面 URL 提取权威标识。
- 普通文件路径或任意网页 URL 不是跨来源的强标识。

弱重复候选使用固定规则生成：

- 标题先做 Unicode NFKC、大小写、连续空白和外围标点规范化。
- 规范标题完全相同且年份/作者没有明显矛盾；或者
- 标题词集合 Jaccard 相似度不低于 0.90、第一作者相同且出版年份相差不超过 1。

阈值只决定是否进入人工复核队列，绝不能触发自动合并。每次候选判断将参与比较的
规范值和分数写入 review case，保证规则可审计。

### 5.2 登记顺序

```text
标准化 PMID、DOI、paper_id 和来源记录键
  -> 分别查询所有非空强标识和已有来源记录映射
  -> 所有命中指向同一行：复用该 literature_id 并补全元数据
  -> 命中多个不同 literature_id：写冲突表，不自动合并
  -> 没有命中且没有弱重复候选：创建 literature_id 并绑定来源记录
  -> 没有命中但存在弱重复候选：创建独立 literature_id、绑定来源记录并标记待确认
```

标题、年份和作者只能生成 `possible_duplicate` 候选，不能单独触发强制复用。
为弱候选创建独立 ID 是为了保证项目结果可引用且同一来源重跑稳定；人工确认相同后再
通过 alias 合并，确认不同时解除 `possible_duplicate` 并允许向量入库。

登记和元数据更新必须在数据库事务中执行。数据库唯一约束是并发下的最终保护；如果
插入遇到唯一约束冲突，流程应重新查询并复用现有行，而不是报错后创建另一个 ID。
来源记录映射已存在但新出现的强标识指向另一行时，也必须进入冲突流程。

### 5.3 元数据合并规则

强标识：

- 现有字段为空时可以补充。
- 新旧非空值不同则进入冲突流程，不能静默覆盖。
- 空值永远不能覆盖非空值。

普通元数据：

- PubMed/MEDLINE 是 PMID、MeSH、publication types 和 PubMed 题录字段的优先来源。
- PubMed/MEDLINE 的 AID/LID 中格式合法的 DOI 可作为强标识；Crossref 校验用于质量
  检查和补充题录，不决定该 DOI 是否存在。
- 本地来源可以补充更完整的正文来源信息，但不能把 OCR 片段当成摘要覆盖 PubMed
  abstract。
- 同一来源的新抓取时间晚于旧快照时才允许更新该来源负责的字段。
- 所有字段更新记录在 `metadata_json.field_sources`，至少包含来源和抓取时间。
- `publication_date` 保存来源给出的规范字符串；日期精度另存
  `metadata_json.publication_date_precision`。
- 已撤稿、勘误或缺少摘要的记录仍可登记，但必须显式标记，不能伪装成完整全文。

## 6. 两个文献地图的职责

### 6.1 `literature_map`

只检索本地 Qdrant：

```text
topic
  -> query embedding
  -> Qdrant 扩大召回
  -> Qwen reranker
  -> 按文献聚合 chunks
  -> 解析强标识并登记/复用全局文献
  -> 输出 local literature_id
```

本地结果用于查找全局文献的顺序为：

1. `paper_id`
2. 从权威 `source_uri` 提取的 PMID、DOI 或专利号
3. 其他现有元数据中的可靠外部标识
4. 已有的 `qdrant:<collection> + document_id` 来源记录映射
5. 标题、年份、作者组成的待确认候选

只有标题相似且没有强标识时，结果可登记为待确认候选，但不能把两篇记录自动合并。
`literature_map` 不调用 PubMed，不触发 PubMed embedding。

本地结果执行两次去重：

1. rerank 后先按现有 `paper_id/document_id/source_uri/title` 聚合 chunks。
2. 登记并解析 alias 后，再按 canonical `literature_id` 最终去重。

第二步用于合并现有 28 篇重复试导入等“不同 document_id、同一真实文献”的结果。
最终排名保留该文献排名最高的代表项，同时在 provenance 汇总所有命中文档和 chunk
数量。

### 6.2 `pubmed_literature_map`

只检索线上 PubMed：

```text
PubMed query
  -> ESearch / ESummary
  -> 可选 EFetch 获取摘要、MeSH 等
  -> PMID / DOI 全局登记
  -> 输出 PubMed literature_id
  -> 检查整个 Qdrant collection
  -> 全库没有强重复或待确认重复时才 embedding/upsert
```

PubMed 可嵌入内容只包括实际取得的标题、摘要、MeSH、publication types 等公开元数据。
报告和界面必须称其为“题录/摘要级内容”，不能称为完整全文。

PubMed 命中已有本地向量时：

- 项目的 PubMed ID 文件仍记录该 `literature_id`。
- provenance 标记 `already_vectorized` 和命中的现有数据集。
- 不再写一套 PubMed 摘要向量。
- 不改变已有 points 的 `project_id`。

两个 workflow 的排名和检索结果始终独立。同一篇文献可以同时出现在 local 和 PubMed
ID 文件中，但解析后的 canonical `literature_id` 必须相同。

## 7. 项目落盘和运行历史

### 7.1 目录结构

```text
research_projects/<project_id>/
├── project.json
├── literature/
│   ├── local/
│   │   ├── literature_ids.csv
│   │   ├── report.md
│   │   ├── citations.bib
│   │   └── runs/
│   │       └── <run_id>/
│   │           ├── literature_ids.csv
│   │           ├── report.md
│   │           └── citations.bib
│   └── pubmed/
│       ├── literature_ids.csv
│       ├── report.md
│       ├── citations.bib
│       └── runs/
│           └── <run_id>/
│               ├── literature_ids.csv
│               ├── report.md
│               └── citations.bib
├── provenance/
│   ├── literature_map/
│   │   └── <run_id>.json
│   └── pubmed_literature_map/
│       └── <run_id>.json
├── code/
├── analysis/
└── figures/
```

`run_id` 必须在并发运行中唯一，格式为高精度 UTC 时间加随机短后缀，例如：

```text
2026-07-27T083012.123456Z_literature_map_a1b2c3d4
```

不能只使用秒级时间。

### 7.2 ID 文件

两个 `literature_ids.csv` 都只有一个字段：

```csv
literature_id
550e8400-e29b-41d4-a716-446655440000
6ba7b810-9dad-41d1-80b4-00c04fd430c8
```

要求：

- 固定路径文件表示该来源最近一次成功运行的结果，不是历次结果的累计并集。
- 对应 `runs/<run_id>/` 文件表示该次运行的精确结果。
- 单个文件内 canonical `literature_id` 不重复。
- 行顺序保持本次检索最终排名；不能为了去重后排序而丢失排名语义。
- local 和 PubMed 命中同一文献时允许分别出现，但 ID 必须相同。
- 标题、作者、摘要等元数据从全局表读取，不写入 ID CSV。

排名、检索分数、query translation、去重依据、向量处理状态等写入该次 provenance。
`report.md` 和 `citations.bib` 从全局元数据按 ID 生成；BibTeX key 使用
`lit_<literature_id去连字符后的前12位>`，因此同一文献在两个来源中保持相同且稳定的
引用 key。

### 7.3 Provenance 最小结构

每次 provenance 至少包含：

- `schema_version`、`workflow`、`run_id`、UTC 开始/结束时间和最终运行状态。
- 原始输入参数和标准化后的有效参数。
- 后端名称、目标 collection、embedding/reranker/profile 版本；不得包含 API key。
- PubMed `query_translation`、query degradation、请求/重试/抓取错误汇总。
- `hits[]`：rank、canonical `literature_id`、本次外部标识、匹配方法、检索分数、
  duplicate 状态和 vector job 状态。
- 输入命中数、去重后文献数、输出 ID 数以及各向量状态数量。
- 每个产物的相对路径、字节数和 SHA-256。
- 错误发生阶段、是否可重试和恢复建议。

provenance 不复制完整摘要或全文，只保留复现检索和解析决策所需的数据。

### 7.4 `project.json`

`project.json` schema 升级到版本 2，继续保留：

- 项目基本信息。
- 追加式 `runs[]`。
- 每次运行独立的 provenance 路径。
- local 和 PubMed 两类固定产物路径。
- 每类最近一次成功的 `run_id`。

示例：

```json
{
  "schema_version": 2,
  "outputs": {
    "local": {
      "latest_run_id": "2026-07-27T083012.123456Z_literature_map_a1b2c3d4",
      "literature_ids": "literature/local/literature_ids.csv",
      "report": "literature/local/report.md",
      "citations": "literature/local/citations.bib"
    },
    "pubmed": {
      "latest_run_id": "2026-07-27T084500.123456Z_pubmed_literature_map_e5f6a7b8",
      "literature_ids": "literature/pubmed/literature_ids.csv",
      "report": "literature/pubmed/report.md",
      "citations": "literature/pubmed/citations.bib"
    }
  }
}
```

更新一类 outputs 时必须保留另一类 outputs、历史 runs 和人工添加的未知字段。

### 7.5 原子提交

每次运行按以下顺序提交文件：

1. 在同一项目文件系统内创建 `.staging/<run_id>/`。
2. 写入并校验全部运行产物和 provenance。
3. 原子 rename 文献产物目录为不可变的 `runs/<run_id>/` 快照。
4. 原子提交 `provenance/<workflow>/<run_id>.json`。
5. 使用临时文件加原子 rename 更新该来源的固定路径文件。
6. 最后原子更新 `project.json`，将该次运行标记为成功和 latest。

多目录 rename 不是一个文件系统事务，因此只有第 6 步成功后该 run 才视为已提交。
如果第 3 至第 6 步中断，下一次启动根据快照、provenance 和 staging 状态完成或回滚
提交；未进入 manifest 的孤立快照不能直接成为 latest。失败运行不得替换 latest。

## 8. Qdrant payload、point ID 和 embedding profile

### 8.1 不增加 payload 字段

PubMed 入库不得增加：

- `literature_id`
- `pmid`
- `doi`
- `origin_project_id`
- `corpus_id`
- `library_scope`
- `project_ids`
- `content_hash`
- embedding/chunk profile 字段

如果通用 importer 固定写入 `source_id`，可以继续使用该现有字段。

### 8.2 PubMed payload

```json
{
  "document_id": "bio_literature:pubmed:PMID-12345678",
  "chunk_id": "bio_literature:pubmed:PMID-12345678:0000",
  "chunk_index": 0,
  "category": "bio_literature",
  "source_type": "pubmed",
  "source_id": "pubmed",
  "source_uri": "https://pubmed.ncbi.nlm.nih.gov/12345678/",
  "paper_id": "PMID:12345678",
  "project_id": "pubmed",
  "title": "Example title",
  "snippet": "Abstract preview",
  "text": "Actual embeddable title, abstract, MeSH and publication metadata",
  "tags": ["pubmed"],
  "is_deleted": false,
  "imported_at": "2026-07-24T00:00:00Z"
}
```

Qdrant 中的 `project_id` 是向量数据集标识，不是
`research_projects/<project_id>`：

```text
data-extract-new    原有 OCR/专利向量数据
pubmed              共享 PubMed 向量数据
```

### 8.3 确定性 ID

```text
document_id = bio_literature:pubmed:PMID-<PMID>
chunk_id    = <document_id>:<四位 chunk_index>
point.id    = UUIDv5(namespace URL, chunk_id)
```

一篇文献可以有多个 chunks 和 points，因此 `point.id` 不能作为文献主键。

### 8.4 `pubmed-v1` embedding profile

第一版固定并记录以下 profile：

```text
profile name:       pubmed-v1
embedding model:    Qwen3-Embedding-4B
chunk max chars:    2000
chunk overlap:      200
point namespace:    UUID URL namespace
```

嵌入文本按固定顺序组装，只包含存在的字段：

```text
Title: ...
Abstract: ...
MeSH: ...
Publication types: ...
Journal: ...
Publication date: ...
```

切块、文本规范化和边界选择必须直接复用现有
`scripts/import_vector_knowledge.py` 的语义，并通过 golden tests 固化。写入前检查
collection 的向量维度和距离配置与 embedding 输出相容。

embedding profile 记录在 SQLite 向量任务和 provenance 中，不写入 Qdrant payload。
profile 变化不得静默覆盖 `complete` points；需要重建时必须作为单独、显式批准的
collection migration 执行。

## 9. Qdrant 全库查重、完整性和幂等

### 9.1 查重范围

查重范围是整个目标 collection，不得限制 `project_id`。依次检查：

1. `paper_id=PMID:<PMID>`。
2. 全局文献中已确认的其他 `paper_id`。
3. Qdrant 现有 `paper_id` 可表达的 DOI、专利号等规范变体。
4. 能从 `source_uri` 精确提取的 PMID、DOI 或专利号。
5. 标题、年份、作者相似候选。

前四项的精确匹配可判定为强重复。第五项只能标记 `possible_duplicate`。
没有 payload index 时允许使用分页 scroll 完成精确检查；不得为了本功能在正式
collection 中未经批准创建索引。

### 9.2 三种查重结果

强标识在任意数据集中已经存在：

- 标记 `already_vectorized`。
- 不 embedding、不 upsert、不改原 points。
- 项目 PubMed ID 文件仍记录命中的 `literature_id`。

全库不存在强重复，也没有待确认标题候选：

- 领取向量任务 lease。
- 组装文本、切块、embedding。
- 只 upsert 缺失的确定性 points。
- 校验全部预期 point 后标记 `complete`。

只能通过标题、年份或作者推测重复：

- 标记 `possible_duplicate`。
- 项目仍记录 `literature_id`。
- 默认不 embedding，等待人工确认。
- 人工确认不是同一篇后才允许继续新建向量。

### 9.3 部分写入恢复

不能因为找到任意一个 `paper_id` point 就把本流程自己的半成品判为完整：

1. 根据登记元数据和 `pubmed-v1` profile 重新生成预期 chunks。
2. 计算全部预期 point ID。
3. 批量查询这些 ID。
4. 已存在的跳过，只 embedding/upsert 缺失 chunks。
5. 所有预期 point 均存在后才写 `complete`。

对于改造前已经存在的本地全文向量，只要强标识精确命中，即按
`already_vectorized` 处理，不尝试用 PubMed 摘要补齐或覆盖本地全文。

## 10. PubMed 网络访问与数据质量

### 10.1 请求规则

- 所有 NCBI 请求携带 `tool`，配置后携带 `email` 和 `api_key`。
- 未配置 API key 时，全进程共享限速不超过 3 请求/秒。
- 配置 API key 时，全进程共享限速不超过 10 请求/秒。
- TCP connect timeout 为 10 秒，单次请求 timeout 为 60 秒。
- 尊重 HTTP `Retry-After`。
- 对 429、5xx 和可重试网络错误使用指数退避和 jitter，默认最多重试 5 次。
- 单条 EFetch 失败不终止整次搜索，但必须写 `fetch_error`。
- ESearch/ESummary 主请求失败时，本次运行失败且不得更新 latest。
- `query_translation`、被 PubMed 丢弃或降级的限定词必须进入 report 和 provenance。

当前 workflow 单次最多返回 50 条。扩大规模时必须实现 ESearch history/WebEnv 分页，
不能只提高 `retmax`。

### 10.2 缓存

PubMed 原始响应可缓存到：

```text
<workspace_root>/.codex-med/cache/pubmed/
```

缓存用于降载和失败重试，不是全局文献事实来源。缓存项必须记录抓取时间，默认 7 天后
重新验证；显式 `force_refresh` 可忽略缓存。解析后的权威字段最终仍写入 SQLite。

### 10.3 特殊记录

- 无摘要：登记题录并标记 `abstract_missing=true`；只在可嵌入文本足够且无重复候选时
  入库。
- 撤稿、勘误、评论或更新版本：保留 publication types 和关联 PMID，不自动排除。
- 不同 PMID 的撤稿声明、勘误或评论默认是独立文献，各有自己的 `literature_id`，通过
  `metadata_json.relations` 关联原文。
- ESummary 有记录但 EFetch 删除/不可用：保留题录并标记状态。
- DOI 无法在 Crossref 找到：保留 PubMed 提供的格式合法 DOI，并记录校验警告。
- Crossref 返回的 DOI 题录与 PubMed 明显冲突：创建标识冲突记录，不静默覆盖，也不
  阻止已经确定 PMID 的项目落盘。

## 11. 跨存储失败恢复

SQLite、项目文件和 Qdrant 无法组成单一事务，因此采用可重放的分阶段流程：

```text
创建 run_id
  -> PubMed/Qdrant 检索
  -> SQLite 事务登记 literature_id
  -> SQLite 领取向量任务 lease
  -> Qdrant 查重、embedding、upsert、完整性校验
  -> 写不可变项目运行快照
  -> 更新固定 latest 产物
  -> 追加 project.json run
```

恢复规则：

- 已登记但 embedding 失败：保留全局元数据，任务为 `failed`；PubMed 检索结果仍提交
  到项目并可成为 latest，provenance 必须明确该文献向量失败。
- Qdrant 已写入但文件失败：按确定性 ID 验证并复用 points，不重复 embedding。
- 只写入部分 chunks：接管 lease 后只补缺失 chunks。
- 项目快照成功但 manifest 失败：从快照 provenance 重建 manifest。
- CSV 成功但 provenance 失败：该次运行不提交为 latest。
- 所有错误记录阶段、可重试性和 `last_error`，不能只返回通用失败。

同一 PubMed 搜索命中后，“写项目 ID”与“是否新建向量”始终是两个独立决定。

## 12. 现有数据初始化和兼容迁移

### 12.1 当前线上基线

已核验基线：

| 指标 | 数量 |
| --- | ---: |
| collection | `medical_knowledge_qwen3_4b` |
| Qdrant points | 50,760 |
| 不同 `document_id` | 316 |
| 不同原始 `source_uri` | 288 |
| 不同 `paper_id` | 288 |
| 缺少 `paper_id` 的 points | 2,376 |
| 重复试入库文献 | 28 |

28 篇文献第一次试导入产生 2,376 个无 `paper_id` points，第二次完整导入使用不同
`document_id` 并增加了 `paper_id`。本次方案不删除或改写这些 points。

### 12.2 初始化要求

初始化工具以只读方式扫描正式 collection，并输出 migration report：

- 288 个 canonical 来源组应创建 288 条 `literatures` 记录。
- 28 个第一次试导入组应映射到第二次完整导入对应的同一 `literature_id`。
- 初始化前后正式 collection point 数保持 50,760。
- 无法可靠映射的记录进入候选/冲突报告，不凭标题自动合并。
- 初始化可重复执行；第二次执行不得增加文献行或改变既有 `literature_id`。

上述数量是当前基线的验收预期。如果执行前线上基线变化，必须先重新生成并人工确认
新的只读基线，不能直接修改预期后继续写入。

### 12.3 旧项目迁移

旧项目可能仍使用：

```text
literature/evidence_table.csv
literature/pubmed_records.csv
literature/report.md
literature/citations.bib
provenance/run.json
```

迁移规则：

- 不删除旧文件。
- 根据 `project.json.runs[].workflow` 和可用 provenance 判断旧产物来源。
- 可明确判断时，复制为对应来源的 `runs/<legacy_run_id>/` 历史快照。
- 无法判断来源时，保存在 `literature/_legacy/` 并生成迁移警告。
- 第一次新 workflow 成功后再创建新的固定路径产物。
- `project.json` 升级到 schema 2 时保留历史 runs 和未知字段。

## 13. 项目删除

删除项目只允许删除解析并校验后的精确目录：

```text
research_projects/<project_id>/
```

默认不删除：

- `.codex-med/literatures.sqlite3`
- PubMed 缓存
- `project_id=data-extract-new` 的原有向量
- `project_id=pubmed` 的共享向量
- 其他项目文件

项目删除命令不得按研究项目 ID 查询或删除 Qdrant。未来如果需要回收无人使用的元数据
或共享向量，必须另建带 dry-run、引用扫描和人工确认的垃圾回收流程。

## 14. 实施顺序

1. 实现 SQLite schema migrations、连接配置和备份工具。
2. 实现并测试统一的 PMID、DOI、paper_id、source URI 标准化。
3. 实现文献登记、元数据合并、冲突记录和 alias 解析。
4. 实现只读 Qdrant 初始化扫描，在本地数据库生成 288 条预期记录及 migration report。
5. 改造 `literature_map`：全局登记、local 目录、run 快照和独立 provenance。
6. 改造 `pubmed_literature_map`：全局登记、PubMed 目录、run 快照和独立 provenance。
7. 将 `project.json` 升级到 schema 2，并实现跨 workflow 的无损合并。
8. 实现 Qdrant 全库强标识查重和 `possible_duplicate` 队列。
9. 实现向量任务 lease、`pubmed-v1` embedding、确定性 upsert 和部分写入恢复。
10. 实现 PubMed 全局限速、重试、缓存和错误分类。
11. 添加单元测试、并发测试、故障注入测试和临时 collection 集成测试。
12. 对旧项目执行非破坏性兼容迁移测试。
13. 生成正式 collection 写入前报告和 Qdrant snapshot。
14. 经人工确认后显式启用正式 collection 写入。

每一阶段必须保持前一阶段测试通过，不能先连接正式写入再补测试。

## 15. 测试矩阵

### 15.1 单元测试

- 所有标识标准化样例和非法输入。
- 新文献登记、已有文献复用、空值补全、非空冲突。
- 多强标识命中不同 ID 时不自动合并。
- alias 解析和人工合并事务。
- metadata 来源优先级和空值不覆盖。
- local/PubMed ID 文件去重且保持排名。
- `project.json` 更新一侧 outputs 时保留另一侧。
- 秒内并发运行生成不同 `run_id`。
- `pubmed-v1` 文本模板、切块边界和 UUIDv5 golden values。

### 15.2 并发和故障注入测试

- 两个进程同时登记同一 PMID，只产生一个 canonical `literature_id`。
- 两个进程同时处理同一新 PMID，只允许一个持有 embedding lease。
- 模拟 lease 持有者崩溃后能接管并只补缺失 points。
- 在 SQLite 登记、embedding、每批 upsert、验证、快照 rename、latest 更新和 manifest
  更新处分别注入失败，重跑后状态一致且不重复 embedding。

### 15.3 临时 Qdrant 集成测试

- 已存在于 `data-extract-new` 的文献：PubMed 项目记录 ID，points 增量为 0。
- 全新 PMID 生成 `n` 个 chunks：第一次 points 增量为 `n`，第二次为 0。
- 手工预置其中一部分确定性 points：运行后只增加缺失数量。
- embedding 服务失败：PubMed ID 和报告仍提交，向量任务为 `failed`，重试恢复后不
  改变 `literature_id`。
- local 和 PubMed 命中同一篇文献：两个文件使用同一 canonical ID。
- 两个 workflow 连续和并发运行：各自 latest、历史快照和 provenance 均不覆盖。
- 删除测试项目：SQLite 文献数和 Qdrant point 数均不变。

## 16. 量化验收标准

实现完成后必须全部满足：

- 初始化产生 288 个 canonical 文献组，28 个重复试导入组映射到对应 canonical ID；
  或在基线变化时提供重新确认的等价报告。
- 初始化和项目迁移期间正式 collection 保持 50,760 points，不发生写入。
- `literature_map` 的网络调用中不存在 PubMed，只更新 `literature/local/`。
- `pubmed_literature_map` 不把本地搜索结果混入排名，只更新
  `literature/pubmed/`。
- 同一项目连续运行两个 workflow，双方固定产物、历史快照和 provenance 均存在。
- 所有 ID CSV 只有一列，文件内无重复 canonical ID。
- local 和 PubMed 命中同一文献时 ID 完全相同。
- 重复登记同一 PMID 100 次后，`literatures` 行数只增加 1。
- 已有强标识文献的 PubMed 入库使 Qdrant point 数增加 0。
- 全新 PMID 第一次增加精确的预期 chunk 数，后续串行或并发重跑增加 0。
- 部分写入恢复后，实际 point ID 集合与预期集合完全相等。
- Qdrant 新 PubMed points 只使用现有字段，`project_id=pubmed`，
  `paper_id=PMID:<PMID>`。
- `literatures` 主表不包含项目路径或 Qdrant point 引用。
- 删除项目后全局文献数、共享向量数和其他项目文件不变。
- 所有正式写入开关关闭时，对正式 collection 的写请求数为 0。
- 正式写入开关关闭或单篇 embedding 失败时，成功取得的 PubMed 文献仍正确登记并写入
  项目 PubMed ID 文件。

## 17. 正式上线保护

正式 collection 写入默认关闭。至少同时满足以下条件才允许启用：

1. 显式指定目标 collection。
2. 设置专用写入开关：

   ```text
   CODEX_MED_PUBMED_VECTOR_WRITES=1
   ```

3. 临时 collection 的完整测试矩阵通过。
4. 初始化和 dry-run 报告经人工检查。
5. 正式 Qdrant collection 已创建可恢复 snapshot。
6. SQLite 数据库已通过 SQLite backup API 生成一致性备份，不能在 WAL 写入期间仅复制
   主数据库文件。

代码不得把“能连接 Qdrant”解释为“获得正式写入授权”。关闭写入开关时仍可完成
PubMed 检索、全局登记、项目 ID 落盘和 dry-run provenance，但必须把
`vector_write_disabled` 明确写入运行结果。
