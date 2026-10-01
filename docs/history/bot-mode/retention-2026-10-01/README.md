# 保留率试运行的留证：flush 组收尾时的 `MEMORY.md`

这是 2026-10-01 那次保留率试运行（`bot-mode-retention-2026-10-01.md`，代码在 `d86b917`，deepseek-flash、32k 窗口，
`scripts/bot-retention.sh` 全默认 knob）**flush 组**的原始产物，从当时会话的临时目录里抄出来，免得被清掉。写完即冻结。

| 文件 | 是什么 |
|---|---|
| `flush-MEMORY.md` | flush 组跑完时 bot `retention` 的 `MEMORY.md`，原样（745 字节，sha256 `50df911d…bf3d5b`）。`## Open threads` 是空的：第 1 次 flush 轮用 `remember remove` 删掉了 Live state 那一行，之后再没写回 |

**没有保存的**：同组的会话日志 `messages.jsonl`（256 行，224,076 字节，sha256
`19a3bda1227656ed3169ef58f56d47bc530cbc2da3588bad7d60593786a03c65`）。它与 `MEMORY.md` 合计约 225 KB，超过了入库的
约 200 KB 上限，所以没有拷。原件在
`/private/tmp/claude-501/-Users-joyqi--herdr-worktrees-iota-bot-mode-v1/63919487-0da6-4645-ab72-bc22e38a6a59/scratchpad/retention/flush/messages.jsonl`，
是临时目录，随时会被清掉；手里还有它的人可以用上面的 sha256 核对是不是同一份。

怎么读：

- 两份交接评审（`bot-mode-handoff-fable.md`、`bot-mode-handoff-opus.md`）说的「flush 之后的记忆」，在 `## Open threads`
  这一段上就是这份：第 1 次 flush 删掉那行之后，后两次 flush 没有再往这一段写。fable 评审 §4 的「重放摘要 #1」实验
  拿它当记忆段；它与摘要 #1 当时看到的是否逐字相同没有核对过（摘要的 prompt 没有落盘）。
- 评审里的「行 N」指 `messages.jsonl` 从 1 起的行号（第 92–95 行是第 1 次 flush 轮，第 96 行是摘要 #1）；
  日志没有保存，这些引用只能对照评审里抄出的原文看。
- 里面的人名、代号、数字都是脚本合成的测验事实，不是真实信息。
