# bot 最小窗口的实测出处：8k 反例

`BOT_MIN_WINDOW`（`src/repl/context/tokens.rs`）定为 32k 的依据。原先是长跑测试里一个 `#[ignore]` 的场景
`a_bot_whose_memory_sits_at_the_soft_threshold_outgrows_an_8k_window`（`tests/repl/bot_longrun.rs`），过度设计评审
第三批（`bot-mode-overdesign-codex.md` §2(c)）把它删掉，结论与出处归档在这里。写完即冻结。

## 场景

`GrowingProvider::new(8_192, SEED).remembering_at(remember_turns())`：与长跑的软阈值场景同一个 fake 模型（每 25 轮
`remember` 一次、只在被要求时整理，`MEMORY.md` 涨到 6 KiB 软阈值附近停住），窗口 8192，2000 轮，24 个随机重启点，
`SEED = 0x5eed_2026_1001`。fake 按「发送字节 / 4」计 input，超窗回 400 `context_length_exceeded`，不回答。

## 两次实测

### db11a05（第二轮评审，`bot-mode-review-codex-2.md` 「长跑测试、32k 与 resume meter 的判断」）

命令：`cargo test --test repl bot_longrun -- --include-ignored --nocapture`（整条命令退出 101，8k 按预期失败）。

- 已回答 842/2000 轮；压缩 marker 208 个，约每 9.6 个尝试轮一个；
- **2668 次调用里 1245 次超窗被拒**（不变量 2 红）；已回答调用最大 input+output 9051；
- 24/24 重启加载视图相同（6a），18 次重启后续调用不同（6b 红）；记忆正文最大 6214 B；
- 失败的不变量：2、3、6b。

### 11082d0（删除前在本分支 HEAD 重跑，2026-10-01）

命令：`cargo test --test repl a_bot_whose_memory_sits_at_the_soft_threshold_outgrows_an_8k_window -- --ignored --nocapture`，
退出 101，12.64 s。输出摘要（原样）：

```
memory at the soft threshold, 8k: 1170 of 2000 turns answered, 2929 calls (28 in no-restart references), 24 drops, log 3691 KiB, 12.613790292s
  [FAILED] 0 every turn saved with its reply: 1155 of 2000 turns saved with a final reply, 1155 user turns in the log, 0 twice; missing or unanswered: [407, 432, 452, 453, …]
  [ok] 1 log append-only: 1672 growths seen, none rewrote a byte
  [FAILED] 2 view ≤ window: 961 of 2929 calls refused as over 8192 tokens {"Flush": 46, "Followup": 71, "Turn": 844} (largest refused: 9729); widest answered call 9241
  [FAILED] 3 compactions ≈ expected: 290 markers (290 summary passes), expected ≈ 411 = growth 891125 / (4096 − 3777 kept + 236 half a turn + 1611 flush); ratio 0.70 (0.85–1.15 accepted), one per 6.9 turns
  [ok] 4 one flush (or flush_skipped) per compaction: 154 with a flush, 136 flush_skipped
  [ok] 5 MEMORY.md ≤ 8 KiB: body up to 6274 bytes, whole file up to 6313 bytes
  [ok] 6a restart loads the view the process held: 24 of 24 drops show the view on both sides, 0 differ
  [FAILED] 6b restart sends what running on would have: 24 restarts compared (1 with a flush queued, 3 with the memory block re-read differently), 19 differ:
drop before #628 (flush queued: false, memory block differs from call None): different calls: without the restart [Turn], with it [Flush, Summary, Turn]
…（19 条，形状相同：运行中的进程只发 [Turn]，重启后先 Flush 再 Summary）
  [ok] 7 startup load: slowest 14.514208ms (bound 2s)
```

两次数值不同（db11a05 之后有 flush 归属、物化等改动），结论相同：

- 压缩后的占用（3777 tokens）已经贴着 4096 的阈值，每压一次只腾出约 300 tokens，几轮就再压一次；
- 带 `remember` 回显（每条结果重复写入的整节，约 6 KiB）的 flush 轮与用户轮超窗被拒，进而有轮次没有回答（不变量 0
  在 11082d0 也红了）；
- 6b 的差异是后果不是原因：被拒的轮次没有落地，运行中的进程没排 flush，而按阈值以上用量恢复的重启会排；32k 下不出现。

## 为什么是窗口下限而不是别的

记忆上限（8 KiB）与 bot 的预留下限（`BOT_RESERVE_TOKENS` 32k）都是定值，不随窗口缩放；窗口小到一定程度，记忆块、
`remember` 回显和压缩保留的 flush 交换本身就填满阈值。所以设一个窗口下限，`iota run` 启动时拒绝更小的窗口。

## 局限

- 只证明这个合成负载在 8k 失败、32k 通过（长跑软阈值场景），**不证明 31999 必败**，也没有扫描窗口边界或改变预留的实验组；
- fake 的计量是 `bytes / 4`，不是真实 tokenizer；
- 以后改最小窗口、记忆上限或预留，必须重新实测，不能把本页当成永久证明。复现：从 11082d0 检出，跑上面的命令。
