# Codex Med Science Workbench 开发说明与后续计划

本文记录本轮围绕 `codex-for-med` 医学科研工作台方向完成的开发内容、当前能力边界、使用方式，以及下一阶段开发计划。

## 背景与目标

本轮开发的目标不是再增加一个普通聊天功能，而是让 `codex-med` 更接近科研工作台形态：

- 模型能主动知道当前可用的医学知识库和数据库。
- 模型能选择合适的 tool 查询向量数据库或 SQL 数据库。
- 科研任务不只停留在聊天回答，而是能形成可复现的项目目录和文件产物。
- 在不引入复杂 UI 的前提下，先提供一个只读科研 workflow，为后续文献地图、证据表、引用管理、图表和分析流水线打基础。

当前实现保持保守策略：所有新增能力都是只读检索和文件输出，不支持通过 Codex tool 直接写入 SQL 或向量数据库。

## 本轮已完成内容

### 1. 修正 `search_vector_knowledge` tool schema 文案

之前 `search_vector_knowledge` 的运行时默认值已经切到新的共享后端，但 tool schema 说明仍然容易让模型误以为默认连接：

- `127.0.0.1:6333`
- `medical_knowledge`

本轮已修正 schema 说明，使它和真实默认运行时一致：

- 默认 Qdrant gateway：共享 `codex-med` vector gateway。
- 默认 collection：`medical_knowledge_qwen3_4b`。
- 默认 embedding：`Qwen3-Embedding-4B`。
- 默认 reranker：`Qwen3-Reranker-4B`。

影响：

- 模型在自动选择 tool 参数时不再倾向旧本地地址。
- 用户无需显式传 `qdrant_url` 和 `collection`，默认即可访问当前部署的新向量库。
- tool 描述会更准确地说明当前链路是：query -> Qwen3 embedding -> Qdrant recall -> Qwen3 reranker -> results。

主要代码位置：

- `codex-rs/core/src/tools/handlers/vector_knowledge_spec.rs`

### 2. 新增医学知识库发现工具：`list_med_knowledge_collections`

新增 tool：

```text
list_med_knowledge_collections
```

设计目的：

让模型在不知道当前项目部署了什么数据库时，可以先调用该工具获取可用后端概览。

当前返回内容包括：

- 向量数据库后端：
  - provider：Qdrant
  - collection：`medical_knowledge_qwen3_4b`
  - points count
  - indexed vectors count
  - vector dimension
  - distance metric
  - 当前 category
  - 当前 source type
  - 推荐使用 tool：`search_vector_knowledge`
- SQL 数据库后端：
  - provider：`codex-med-sql-gateway`
  - table：`antibodies`
  - row count
  - 推荐使用 tool：`query_antibody_training_records`

典型用途：

```text
先列出当前 codex-med 有哪些医学知识库和数据库，再决定用哪个 tool 查询。
```

这个工具解决的是“模型不知道有什么库”的问题。它相当于科研工作台里的 data catalog 初版。

主要代码位置：

- `codex-rs/core/src/tools/handlers/science_workbench.rs`
- `codex-rs/core/src/tools/handlers/science_workbench_spec.rs`

### 3. 新增数据库自描述工具：`describe_med_database`

新增 tool：

```text
describe_med_database
```

参数：

| 参数 | 默认值 | 说明 |
| --- | --- | --- |
| `include_schema` | `true` | 是否返回 SQL 表结构和字段说明 |
| `include_vector_details` | `true` | 是否返回 Qdrant collection 和 payload 字段说明 |

设计目的：

让模型不仅知道“有数据库”，还知道“数据库里是什么、适合问什么、不能做什么”。

当前返回内容包括：

- SQL 数据库概要：
  - 抗体元数据
  - 抗体名称
  - 靶点
  - CDR-H3
  - VH/VL 序列
  - KD / kon / koff
  - EC50
  - patent / paper provenance
- 向量数据库概要：
  - collection 信息
  - category 计数
  - source type 计数
  - payload 字段约定
- 推荐使用方式：
  - 精确结构化查询走 `query_antibody_training_records`
  - 语义检索走 `search_vector_knowledge`
  - 文献证据地图走 `literature_map`
- 写入能力说明：
  - 当前 Codex 内置 tools 为只读。
  - 数据新增应通过离线 ingestion/admin pipeline 完成。

典型用途：

```text
描述当前医学 SQL 和向量数据库，告诉我里面有什么字段，适合查询什么问题。
```

这个工具是后续 agent 自动规划任务的基础。没有它，模型只能靠提示词或记忆猜数据库结构。

主要代码位置：

- `codex-rs/core/src/tools/handlers/science_workbench.rs`
- `codex-rs/core/src/tools/handlers/science_workbench_spec.rs`

### 4. 新增只读科研 workflow：`literature_map`

新增 tool：

```text
literature_map
```

参数：

| 参数 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `topic` | 是 | 无 | 科研主题或问题 |
| `project_id` | 否 | 从 topic 生成 slug | 研究项目目录名 |
| `year_range` | 否 | 无 | 记录到 report 中的年份范围标签 |
| `top_k` | 否 | `12` | 收集的证据 chunk 数，最大 30 |
| `category` | 否 | `bio_literature` | 向量库 category 过滤 |

设计目的：

先实现一个最小可用的科研 workflow：

1. 根据 topic 生成 embedding。
2. 查询当前 Qdrant 文献向量库。
3. 收集 top-k evidence chunks。
4. 自动生成标准研究项目目录。
5. 输出 evidence table、report、BibTeX 和 provenance。

生成目录结构：

```text
research_projects/<project_id>/
  literature/
    evidence_table.csv
    report.md
    citations.bib
  provenance/
    run.json
  figures/
  analysis/
```

其中：

- `evidence_table.csv`：结构化证据表，便于人工筛选和后续分析。
- `report.md`：初版文献地图报告，包含主题、年份范围、证据列表和初步空白点。
- `citations.bib`：从 evidence payload 中提取的引用骨架。
- `provenance/run.json`：记录 workflow、topic、参数、后端、输出文件和证据数量。
- `code/`：预留给后续模型训练、模型评测、数据预处理 pipeline 和 benchmark 代码。
- `figures/`：预留给后续机制图、统计图、路径图。
- `analysis/`：预留给后续 Python/R notebooks、脚本和中间结果。

典型调用意图：

```text
请用 literature_map 为 integrated stress response aging neurodegeneration 生成一个文献证据地图，年份范围 2015-2026，取 top 20。
```

这个 workflow 目前是“只读 evidence map”，不是完整的 PubMed 自动综述系统。它先基于当前已部署的向量库生成可复现产物，后续可以叠加外部数据库检索、去重、引用校验和多 agent 协作。

主要代码位置：

- `codex-rs/core/src/tools/handlers/science_workbench.rs`
- `codex-rs/core/src/tools/handlers/science_workbench_spec.rs`

### 5. 建立 `research_projects/` 标准输出约定

本轮将科研任务输出目录标准化为：

```text
research_projects/<project_id>/
```

这个目录不是简单缓存，而是科研工作台的项目产物根目录。后续所有科研 workflow 都应优先落盘到这里，而不是只返回聊天文本。

推荐长期目录约定：

```text
research_projects/<project_id>/
  literature/
  data/
  code/
  analysis/
  figures/
  manuscript/
  provenance/
```

各目录职责：

| 目录 | 功能 | 典型文件 |
| --- | --- | --- |
| `literature/` | 文献相关产物。用于保存文献检索结果、证据表、文献地图、综述报告草稿和引用文件。 | `evidence_table.csv`、`report.md`、`citations.bib` |
| `data/` | 原始数据和处理后数据。用于保存用户上传、外部数据库下载或 workflow 生成的数据集。 | `raw/counts.csv`、`raw/metadata.csv`、`processed/normalized_counts.csv` |
| `code/` | 项目级可复用代码。用于保存模型训练、模型评测、数据预处理 pipeline、特征工程、benchmark 和工具库代码。 | `train.py`、`evaluate.py`、`src/dataset.py`、`src/metrics.py` |
| `analysis/` | 项目分析脚本、中间结果和统计输出。用于保存一次性分析 notebook、探索性分析、差异分析、通路富集和 signature scoring 结果。 | `notebooks/exploration.ipynb`、`results/differential_expression.csv`、`results/pathway_enrichment.csv` |
| `figures/` | 图表和机制图。用于保存可进入论文、综述或汇报的可视化结果。 | `figure_1.pdf`、`isr_pathway.svg`、`volcano_plot.png` |
| `manuscript/` | 写作产物。用于保存综述大纲、论文草稿、摘要、图注、cover letter 和审稿回复。 | `outline.md`、`draft.md`、`figure_legends.md` |
| `provenance/` | 可复现记录和审计信息。用于保存 workflow 参数、数据来源、工具调用、模型版本、运行时间和输出文件索引。 | `run.json`、`agent_runs.jsonl`、`project.json` |

可以把这些目录理解为：

```text
literature   = 查了什么文献、提取了什么证据
data         = 用了什么原始/处理数据
code         = 训练、评测和 pipeline 代码怎么写
analysis     = 怎么分析的
figures      = 生成了什么图
manuscript   = 怎么写成论文/综述
provenance   = 整个过程如何复现和审计
```

本轮已实现：

- `literature/`
- `code/`
- `analysis/`
- `figures/`
- `provenance/`

暂未实现但建议后续补齐：

- `data/`
- `manuscript/`

## 当前工具能力矩阵

| Tool | 数据源 | 查询能力 | 写入数据库 | 文件落盘 | 主要用途 |
| --- | --- | --- | --- | --- | --- |
| `query_antibody_training_records` | SQL antibody DB | 支持，只读 SQL | 不支持 | 不负责 | 精确查询抗体、靶点、序列、KD、EC50 |
| `search_vector_knowledge` | Qdrant vector DB | 支持，语义检索 | 不支持 | 不负责 | 查文献、OCR markdown chunks、上下文证据 |
| `list_med_knowledge_collections` | SQL + Qdrant metadata | 支持，后端发现 | 不支持 | 不负责 | 让模型知道当前有哪些库 |
| `describe_med_database` | SQL schema + Qdrant metadata | 支持，自描述 | 不支持 | 不负责 | 让模型知道库里有什么字段和用途 |
| `literature_map` | Qdrant vector DB | 支持，按 topic 收集 evidence | 不支持 | 支持 | 生成科研项目证据包 |

## 当前数据库状态理解

基于前面实际检查和 tool 设计，当前系统里主要有两类数据。

### SQL 数据库

SQL 数据库目前面向抗体训练数据和结构化记录，核心表为：

```text
antibodies
```

主要信息类型：

- paper / patent 标识
- document title
- document category
- antibody name
- antibody type
- antibody isotype
- source
- target name
- target type
- cross reactivity
- epitope
- experiment
- KD / kon / koff
- EC50
- mechanism of action
- quantitative metric
- CDR-H3 sequence
- VH sequence
- VL sequence
- thermal stability
- in vivo half life
- in vivo efficacy
- reference source

适合问题：

- 某个靶点有哪些抗体？
- 哪些记录包含 CDR-H3？
- 某个 patent/paper 中有哪些 antibody？
- 某类抗体的 KD/EC50 如何？
- 是否存在特定 VH/VL/CDRH3 序列？

不适合问题：

- 开放式机制综述。
- 大段文献语义理解。
- 多论文观点综合。

这类问题更适合用向量库或 `literature_map`。

### 向量数据库

向量数据库当前使用 Qdrant，核心 collection 为：

```text
medical_knowledge_qwen3_4b
```

当前主要信息类型：

- OCR 修复后的 biomedical markdown chunks
- 文献或专利来源片段
- `bio_literature` category
- `ocr_repaired_markdown` source type
- title / snippet / text / source_uri / citation 等 payload

适合问题：

- 语义检索某个医学、生物学或抗体主题。
- 从文献片段中找证据。
- 为综述、研究假设、机制分析提供上下文。
- 根据 topic 收集 evidence chunks。

不适合问题：

- 精确统计抗体表字段。
- 需要事务、排序、聚合的结构化查询。
- 直接新增或删除数据。

这类结构化问题应走 SQL tool。

## 和 Claude Science 方向的对应关系

Claude Science 的核心不是“更会聊天”，而是把科研工作流产品化。本轮改造对应关系如下：

| Claude Science 能力 | 当前 codex-med 对应实现 | 当前阶段 |
| --- | --- | --- |
| 科研数据库连接 | SQL antibody DB + Qdrant vector DB | 已有 |
| 工具自发现 | `list_med_knowledge_collections` | 初版完成 |
| 数据库自描述 | `describe_med_database` | 初版完成 |
| 文献地图 | `literature_map` | 初版完成 |
| 可复现记录 | `research_projects/<project_id>/provenance/run.json` | 初版完成 |
| 文件产物 | evidence CSV、report、BibTeX | 初版完成 |
| 多 agent 协作 | 尚未实现专用科研 agents | 待开发 |
| 外部数据库 connectors | 尚未接 PubMed/GEO/UniProt workflow | 待开发 |
| HPC / 远程计算管理 | 尚未实现 | 待开发 |
| 图表和论文生成 | 仅预留目录 | 待开发 |

因此，当前 `codex-med` 已经从“能查数据库的 Codex”迈向“轻量科研 workbench 初版”，但还没有达到完整 AI for Science 平台。

## 与 Claude Science 的主要差距

说明：本节基于截至 2026-07-08 可见的公开报道和产品描述整理。公开资料将 Claude Science 描述为 Anthropic 面向科研人员的 beta AI workbench，重点是把文献、数据分析、代码环境、图表、可审计记录和本地/HPC 基础设施整合到统一科研环境。后续官方能力可能继续变化，因此这里的对比应作为产品规划参考，而不是固定规格。

参考公开信息：

- TechRadar, 2026-07-04: `Anthropic launches "AI workbench" for scientists using Claude`
- The Verge, 2026-07-03: `Anthropic wants to develop its own drugs`

### 总体差距概览

| 维度 | Claude Science 公开定位 | 当前 `codex-med` 状态 | 差距判断 |
| --- | --- | --- | --- |
| 产品形态 | 面向科学家的独立 beta workbench app，覆盖科研全流程 | 基于 Codex CLI/npm 包的医学工具增强版 | 缺少独立科研产品界面和完整工作台体验 |
| 科研流程覆盖 | 文献调研、假设探索、数据分析、图表生成、论文写作、发表辅助 | 已有数据库查询、向量检索、`literature_map` 初版 | 目前只覆盖检索和初级 evidence map |
| 数据库连接 | 公开描述强调 PubMed、Jupyter、R、科研工具链整合 | 已接 SQL antibody DB、Qdrant medical vector DB | 缺少 PubMed/GEO/SRA/UniProt/Crossref 等外部科研 connectors |
| 计算环境 | 可在实验室基础设施、Linux 机器或 HPC login nodes 上运行 | 当前主要依赖本机 Codex 执行和内置 tools | 缺少 HPC/Slurm/远程任务管理 |
| 数据分析 | 面向单细胞 RNA-seq、CRISPR screen、蛋白结构、化学信息学等场景 | 尚未内置组学分析 workflow | 缺少标准 bioinformatics pipelines |
| 图表生成 | 强调科学图表和视觉产物生成，并保留代码和过程 | 目前仅预留 `figures/` 目录 | 缺少 figure agent、绘图模板和可复现图表代码 |
| 可审计性 | 公开描述强调 source code、message history、plain-language explanations | 已有 `provenance/run.json` 初版 | 需要更完整的 run history、文件 hash、tool trace、模型版本记录 |
| 多 agent 协作 | 定位上更接近多 specialist agents 协同科研 | 当前新增的是单 tool workflow | 缺少 Literature/Analysis/Figure/Citation/Reviewer agents |
| 数据隐私 | 公开描述强调在用户实验室基础设施上运行，敏感数据不离开原系统 | 当前可以本机运行，但 embedding/rerank 和网关依赖外部服务 | 需要本地模型、本地向量库和私有部署模式 |
| 项目管理 | 目标是统一科研项目工作区 | 已定义 `research_projects/<project_id>/` | 还缺 project manifest、run registry、任务状态和 UI |
| 数据写入和 ingestion | 目标覆盖完整科研资料管理 | 当前 tools 刻意保持只读 | 需要受控 ingestion workflow，而不是开放裸写 API |
| 发表辅助 | 公开描述覆盖 manuscript drafting / publication | 仅预留 `manuscript/` 目录 | 缺少大纲、草稿、引用校验、期刊格式化 workflow |
| 生命科学深度 | 公开案例包括 single-cell、CRISPR、protein structure、cheminformatics | 当前更偏抗体数据库和医学文献检索 | 缺少专科 workflow 和领域知识包 |
| 生态集成 | Claude/MCP/科研工具生态整合 | 当前是 Codex 内置 Rust tools | 缺少 MCP server、第三方平台复用和插件市场化 |

### 关键差距 1：从 CLI tool 到独立科研工作台

当前 `codex-med` 的优势是轻量、可通过 npm 安装、直接嵌入 Codex tool runtime。但它仍然主要是命令行和内置工具形态。

Claude Science 的方向更像独立 app：

- 有清晰的科研工作区。
- 能组织多阶段任务。
- 能管理数据、代码、图表、论文草稿。
- 用户不需要理解底层 tool 参数，也能围绕科研目标工作。

`codex-med` 后续如果要靠近这个方向，需要补齐：

- 项目首页或 TUI/web UI。
- 项目目录浏览。
- workflow run 列表。
- evidence table 预览和人工校正。
- 图表预览。
- manuscript 草稿管理。

### 关键差距 2：外部科研数据库 connectors 不足

当前 `codex-med` 已有两个核心数据源：

- SQL antibody database
- Qdrant medical vector database

这适合查询已有内部数据，但还不等于科研人员每天需要的外部知识网络。

与 Claude Science 方向相比，下一步最缺的是：

- PubMed / NCBI E-utilities
- Europe PMC
- Crossref
- Semantic Scholar
- UniProt
- GEO / SRA
- ClinicalTrials.gov
- PDB / AlphaFold DB

没有这些 connectors，`literature_map` 只能从现有向量库里找证据，不能主动构建最新、完整、可引用的文献 corpus。

### 关键差距 3：科研数据分析 pipeline 尚未产品化

当前 `codex-med` 可以写代码，也已经定义了：

```text
data/
code/
analysis/
figures/
```

但还没有把常见生命科学分析流程封装为 workflow。

优先应补：

- bulk RNA-seq differential expression
- pathway enrichment
- gene set scoring
- single-cell marker summary
- antibody sequence quality control
- antibody affinity benchmark
- model training/evaluation pipeline

这类能力应该输出：

- 标准结果表
- 可复现代码
- 环境文件
- 图表
- provenance
- 自动生成的解释报告

### 关键差距 4：可复现和审计还不够完整

当前已有：

```text
provenance/run.json
```

但完整科研审计需要更多信息：

- 输入文件 hash
- 输出文件 hash
- tool 调用参数
- tool 返回摘要
- model/provider/version
- embedding/reranker model
- 数据库 collection/table 版本
- git commit
- 环境信息
- 执行时长
- 人工修改记录

后续建议扩展为：

```text
provenance/
  project.json
  runs/
    2026-07-08T120000Z_literature_map.json
  tool_calls.jsonl
  files.manifest.json
```

### 关键差距 5：多 agent 科研协作还未形成

当前 `literature_map` 是单个 tool 完成一个线性任务。

Claude Science 方向更接近：

```text
科研目标
  -> 自动拆解
  -> 多 specialist agents 并行/串行完成
  -> 统一汇总
  -> 人类审查
```

`codex-med` 后续应逐步引入：

- Literature Agent
- Database Agent
- Bioinformatics Agent
- Model Training Agent
- Figure Agent
- Citation Agent
- Reviewer Agent

每个 agent 都应写入 `provenance/agent_runs.jsonl`，避免只产生不可追踪的聊天结论。

### 关键差距 6：HPC 和远程计算管理缺失

生命科学工作流经常需要：

- 大型 FASTQ/BAM/count matrix
- GPU 训练
- 单细胞数据处理
- Slurm/PBS 作业
- 远程服务器
- 长时间后台任务

当前 `codex-med` 还没有：

- HPC profile
- Slurm job submit/check/cancel
- remote SSH execution manager
- job queue
- background run resume
- 大文件输入输出策略

如果要接近科研工作台，这部分必须独立设计，不能只依赖普通 shell tool。

### 关键差距 7：本地隐私部署还不完整

Claude Science 的公开描述强调可在实验室基础设施上运行，避免大型敏感数据离开原系统。

当前 `codex-med` 虽然可以在本服务器运行，但默认链路仍可能使用：

- 共享 SQL gateway
- 共享 Qdrant gateway
- 外部 embedding endpoint
- 外部 reranker endpoint

对医院、药企或未公开实验数据来说，需要支持：

- 本地 Qdrant
- 本地 PostgreSQL
- 本地 embedding/reranker model
- 私有对象存储
- 无外网模式
- per-project credential isolation

### 关键差距 8：从“可用工具”到“可交付科研结果”

当前 `codex-med` 已经能查询数据并生成初步 evidence package，但离可交付科研结果还有距离。

更完整的交付应包括：

- evidence table
- citation-validated bibliography
- reproducible analysis code
- generated figures
- manuscript outline
- claims-to-evidence map
- limitations and risk notes
- reviewer checklist
- export package

这也是后续 `research_projects/` 目录要持续扩展的原因。

### 差距收敛路线

建议按以下顺序追赶，而不是一次性重做成大平台：

1. **巩固数据底座**：统一配置、project manifest、provenance、live schema/category discovery。
2. **补外部文献 connectors**：PubMed、Europe PMC、Crossref、Semantic Scholar。
3. **升级 `literature_map`**：外部检索、去重、引用校验、evidence grading。
4. **建立项目级 corpus**：PDF/markdown/CSV ingestion、chunking、embedding、project search。
5. **引入分析 workflows**：RNA-seq、pathway enrichment、single-cell summary、抗体模型评测。
6. **加入图表和 manuscript workflows**：figure code、figure legends、review outline、journal formatting。
7. **引入多 agent 编排**：literature/database/analysis/figure/citation/reviewer 分工。
8. **支持私有部署和 HPC**：local models、local vector DB、Slurm、remote execution、background jobs。

结论：

`codex-med` 当前最接近 Claude Science 的部分是“工具化数据库接入 + 可复现项目目录 + 初版 literature workflow”。最大差距在“外部科研生态连接、数据分析 workflow、图表/论文产物、多 agent 编排、HPC/隐私部署和产品化工作台体验”。

## 当前限制

### 1. 新增工具仍是内置 Rust tool，不是独立 MCP 服务

优点：

- npm 安装后的 `codex-med` 可以直接获得内置 tool。
- 用户不需要额外配置 MCP server。
- 模型调用路径更短。

限制：

- 工具升级需要重新构建和发布 npm 包。
- 第三方系统不能直接通过 MCP 复用这些能力。

后续可以考虑同时提供 MCP gateway，把同一套能力暴露给 Claude Desktop、Cursor、其他 agent 平台。

### 2. `literature_map` 目前只查已有向量库

当前不会主动联网检索 PubMed、Europe PMC、Crossref 或 Semantic Scholar。

这意味着：

- 如果向量库没有收录某篇论文，它不会出现在 evidence map。
- citations.bib 依赖已有 payload，可能只是引用骨架。
- report 是 evidence map 初稿，不应直接当作最终综述。

后续应增加外部文献 connector 和引用校验。

### 3. 当前 tools 不支持数据库写入

这是刻意设计。

原因：

- 避免模型误写生产数据库。
- 避免未经审核的数据污染向量库。
- 保持当前 npm 包默认安装后的安全性。

如果后续要支持新增数据，应设计单独 ingestion workflow，并加入：

- 数据校验
- provenance
- dry-run
- 权限控制
- 审核确认
- rollback 或 soft delete

### 4. 科研 workflow 还没有任务状态管理

当前 `literature_map` 是一次性执行：

```text
输入 topic -> 查询 -> 生成文件 -> 返回路径
```

还没有：

- task queue
- long-running job
- incremental run
- resume
- result diff
- project manifest

后续做复杂组学分析或多 agent workflow 时需要补齐。

## 后续开发计划

### 第一阶段：完善当前 Science Workbench 基座

目标：让现有 SQL、向量库和 literature workflow 更稳、更自解释。

建议任务：

1. 将后端配置常量统一抽取，避免多个 handler 重复维护 URL、collection、model 名称。
2. 给 `describe_med_database` 增加 live category/source type 自动聚合，不再硬编码已知 category。
3. 给 `literature_map` 增加 reranker 阶段，使 evidence 排序和 `search_vector_knowledge` 保持一致。
4. 给 `literature_map` 增加去重逻辑，按 paper_id、source_uri、title 聚合同一文献的多个 chunk。
5. 给 `research_projects/<project_id>/` 增加 `project.json` manifest，记录项目主题、创建时间、workflow runs 和产物索引。
6. 增加集成测试或 mock server 测试，覆盖 SQL schema、Qdrant count、Qdrant search、文件生成。

预期结果：

- 模型能稳定发现数据库。
- 文献地图质量更高。
- 研究项目目录具备长期可管理性。

### 第二阶段：接入外部科研数据库 connectors

目标：从“查本地已有向量库”升级为“主动构建文献证据集”。

建议 connectors：

- PubMed / NCBI E-utilities
- Europe PMC
- Crossref
- Semantic Scholar
- UniProt
- GEO / SRA
- ClinicalTrials.gov

建议新增 tools 或 workflow：

```text
search_pubmed_literature
fetch_pubmed_record
build_literature_corpus
validate_citations
```

`literature_map` 的推荐升级流程：

```text
topic
  -> external literature search
  -> metadata fetch
  -> abstract/full-text chunking
  -> optional embedding and local project index
  -> evidence table
  -> citation validation
  -> report
```

预期结果：

- 不依赖当前向量库是否已收录。
- citations.bib 更可靠。
- 可以按年份、物种、疾病、细胞类型、证据类型筛选。

### 第三阶段：科研项目级 RAG 和私有知识库

目标：每个 `research_projects/<project_id>` 都能形成自己的可检索知识空间。

建议能力：

- 项目级 corpus：
  - PDFs
  - markdown
  - notes
  - CSV
  - external records
- 项目级向量索引：
  - collection 或 payload `project_id`
  - 支持增量 embedding
  - 支持 soft delete
- 项目级 provenance：
  - 每次 ingestion 的来源、hash、chunk 参数、embedding model

建议新增 workflow：

```text
ingest_research_documents
index_research_project
search_research_project
```

注意：

这个阶段可以开始支持“增加数据”，但不建议直接让普通模型调用裸写入 API。应通过受控 ingestion workflow 完成。

### 第四阶段：生命科学分析 workflow

目标：让平台从文献工作台扩展到数据分析工作台。

建议先支持：

- bulk RNA-seq
- differential expression
- pathway enrichment
- gene set scoring
- ISR signature scoring
- single-cell marker summary

建议目录：

```text
research_projects/<project_id>/
  data/
    raw/
    processed/
  code/
    src/
    configs/
    tests/
  analysis/
    notebooks/
    results/
  figures/
  provenance/
```

建议新增 workflows：

```text
rnaseq_differential_expression
pathway_enrichment
score_gene_signature
single_cell_marker_map
```

对于 ISR / aging / neurodegeneration 方向，可以优先内置 gene sets：

- ISR core genes
- PERK-eIF2alpha-ATF4 axis
- unfolded protein response
- senescence markers
- neuroinflammation markers
- cell-type marker sets

### 第五阶段：科研 agent 协作

目标：从单 tool workflow 发展成多 agent 科研流程。

建议 specialist agents：

- Literature Agent：检索文献、去重、提取证据。
- Database Agent：查询 SQL、UniProt、GEO 等结构化数据库。
- Analysis Agent：运行 Python/R 分析。
- Figure Agent：生成图表和机制图草稿。
- Citation Agent：校验引用和 BibTeX。
- Reviewer Agent：检查证据强度、遗漏和过度推断。

典型 workflow：

```text
用户目标：
  Build a literature map of ISR, aging, and neurodegeneration from 2015-2026.

Orchestrator:
  -> Literature Agent: collect papers and evidence
  -> Database Agent: enrich genes/proteins/diseases
  -> Analysis Agent: summarize evidence matrix
  -> Citation Agent: validate references
  -> Reviewer Agent: identify gaps and weak claims
  -> Figure Agent: draft pathway figure
```

预期产物：

```text
research_projects/isr-aging-neurodegeneration/
  literature/evidence_table.csv
  literature/report.md
  literature/citations.bib
  figures/pathway_model.svg
  manuscript/outline.md
  provenance/agent_runs.jsonl
```

### 第六阶段：发布和产品化

目标：让用户通过 npm 安装后稳定使用。

建议发布前检查：

1. 确认所有新 Rust files 被纳入构建。
2. 运行 core tests。
3. 构建 npm 包二进制。
4. 用全新目录实际测试：
   - `codex-med exec` 能看到新 tools。
   - `list_med_knowledge_collections` 能返回后端信息。
   - `describe_med_database` 能返回 schema。
   - `literature_map` 能生成 `research_projects/` 文件。
5. 更新 npm 版本号。
6. 发布 npm。
7. 推送 GitHub 分支或 PR。

建议发布说明重点：

- 新增 Science Workbench tools。
- 新增 database discovery。
- 新增 read-only literature map workflow。
- 新增 `research_projects/` reproducible output。
- 说明当前仍为只读，不支持直接写数据库。

## 推荐近期优先级

如果按投入产出比排序，建议下一步优先做：

1. 发布当前版本，让 npm 安装用户先能用到三个新 tools。
2. 给 `literature_map` 加 reranker 和文献去重。
3. 给 `research_projects/` 增加 `project.json`。
4. 接 PubMed / Europe PMC connector。
5. 增加 project-level ingestion workflow。

这条路线能最快把 `codex-med` 从“医学数据库查询工具”推进到“可复现医学科研工作台”。

## 验证记录

本轮已运行并通过：

```bash
cargo +1.95.0 test --manifest-path codex-rs/Cargo.toml -p codex-core science_workbench
cargo +1.95.0 test --manifest-path codex-rs/Cargo.toml -p codex-core vector_knowledge_spec
cargo +1.95.0 test --manifest-path codex-rs/Cargo.toml -p codex-core spec_plan
git diff --check
```

说明：

- 测试过程中出现过只读文件系统导致无法更新 PATH 的 warning，但测试本身通过。
- 当前文档和代码改动尚未等同于 npm 发布；npm 用户需要安装包含这些 commits 的新版本后才能使用新增 tools。
