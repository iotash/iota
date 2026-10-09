# bot 模式：冻结的评审与验收记录

这里是 bot 模式 v1（`bot-mode-v1` 分支）从设计到合并前产生的过程文档，写完即冻结，不再随代码更新。现行的设计与设计依据留在 `docs/design/`：`bot-mode.md`（设计，以它为准）与 `bot-mode-research.md`（成熟产品调研，设计依据）。

| 文件 | 是什么 |
|---|---|
| `bot-mode-recon.md` | 设计前的代码勘察（设计 rev 2 里称 recon） |
| `bot-mode-critique.md` | 对设计 rev 1 的独立评审（设计里称「评审」，编号 S*/B*/I*） |
| `bot-mode-review-fable.md`、`bot-mode-review-codex.md` | 实现的第一轮评审 |
| `bot-mode-review-fable-2.md`、`bot-mode-review-codex-2.md` | 修复后的第二轮评审 |
| `bot-mode-verify-codex.md` | 修复验收与放行 |
| `bot-mode-overdesign-{fable,opus,codex}.md` | 合并前的三份过度设计评审（codex 那份含仲裁结论） |
| `bot-mode-8k-evidence.md` | `BOT_MIN_WINDOW` 的实测出处：删掉的 8k 长跑反例的命令、提交与输出摘要 |
| `bot-mode-retention-2026-10-01.md` | 保留率实验第一次用真模型跑（deepseek-flash，32k）；§4.2 记下了 flush 与摘要的交接缺口 |
| `bot-mode-handoff-{fable,opus}.md` | 对那个交接缺口四种修法的两份独立评审；结论都是只修计数（删除不算存），见设计 §7.1 |
| `retention-2026-10-01/` | 那次试运行 flush 组收尾时的 `MEMORY.md` 原件；会话日志超过入库上限，只记了大小与 sha256 |
| `bot-memory-scope-project-superseded.md`、`bot-memory-scope-two-layer-superseded.md` | 记忆作用域的两份对照方案（2026-10-07；项目键分别取根提交与 git common dir）。2026-10-09 所有者决定项目键用 `project_slug`，两份被 `docs/design/bot-memory-scope.md` 取代；它的 §2.4 逐条列出从这两份里删掉了什么 |

读的时候注意：

- 文中的 `file:line` 指各自写作时的 HEAD（每份开头写明了提交），与现在的代码不一定对得上；核对时先 `git show <提交>:<路径>`。
- 文中以裸文件名互相引用的，是本目录的兄弟文件；提到 `bot-mode.md`、`bot-mode-research.md` 的，在 `docs/design/`。
