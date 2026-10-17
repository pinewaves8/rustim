# sdks/typescript —— 协议的第三种语言实现

这是 `驱动协议v1.md` 的 Node 侧薄客户端，以及一套用它驱动**真实二进制**的一致性验收。
它与 `sdks/python/` 是同一个问题的第三次提问：

> 一个只读协议文档、与引擎不共享任何代码的客户端，拿到的是不是同一批答案？

三种语言（Rust 的 `rustim-client`、Python、JavaScript）各自独立实现同一组帧规则，并各自驱动
真实二进制通过同一组检查——“自由切换”才不只是 Rust 内部的自证。

## 文件

| 文件 | 内容 |
|---|---|
| `rustim-client.mjs` | 同步风格的异步客户端。只管帧规则：一行命令 → 一行回复，`seq` 等于物理行号，回复里 `diff` / `tree` / `report` / `plan` / `capabilities` / `error` 恰有其一 |
| `conformance.mjs` | 一致性验收：握手词汇表、原语命令面、讲解计划的装/步/回退、两个独立进程重放同一日志得到同一棵语义树 |

两者都**只依赖 Node 标准库**（`node:child_process`、`node:readline`、`node:fs`，
ES module），不需要 `npm install`、不需要 `node_modules`、不联网。要求 Node 20+（本仓库在
Node 24 上验证）。

## 用法

```bash
cargo build -p rustim-app
node sdks/typescript/conformance.mjs
node sdks/typescript/conformance.mjs --binary target/release/rustim-app
```

退出码：`0` 全部通过，`1` 有检查失败（失败行会写明命令、期望形状和实际回复），
`2` 问不出问题（没有可执行的二进制）。

自己写客户端时：

```js
import { Client } from "./rustim-client.mjs";

const client = Client.spawn("target/debug/rustim-app",
  ["--script", "-", "--reply", "--png", "/tmp/out.png"]);
const caps = await client.hello();            // { protocol: 1, actions: [...], ... }
const reply = await client.send({ action: "add", type: "circle",
  params: { radius: 1.0, label: "圆" } });
const circle = reply.diff.created[0];         // created 是学会新 id 的唯一途径
await client.close();
```

`--script -`（stdin）必须与 `--png`、`--inspect` 或视频模式之一配对，否则进程会落到窗口事件
循环里；`--reply` 与 `--inspect` 不能同时占用 stdout。

## 客户端必须遵守的协议规则

1. **一行命令，一行回复。** 空行是跳过，不产生回复，但计入 `seq` 的物理行号。
2. **回复恰有六种存在之一**：`diff` / `tree` / `report` / `plan` / `capabilities` / `error`。
   缺席的字段不写，绝不写 `null`。
3. **`seq` 是 1 起的物理行号**，客户端应核对自己发到了第几行。
4. **拒绝是值，不是断线。** 引擎拒绝一条命令后 feed 继续；客户端应把 `error` 交给模型，让它
   改下一行。
5. **命令必须能逐字节往返。** 日志、重放、比对都依赖这一点；客户端不要“顺手”重排 JSON 字段。

`rustim-client.mjs` 把 1、2、3 实现为断言：违反帧规则时抛 `ProtocolError`，而不是把畸形流当成
回复返回。引擎的拒绝（例如 `no_such_label`）不抛异常，原样交给调用者。

## 一致性验收检查什么

与 `sdks/python/conformance.py` 完全相同：

- **词汇表**：`hello` 报出的 36 个动作、45 条缓动、两个 profile 与这份构建实际接受的完全一致；
  教学档的布局/注意力/计划动词都在其中。
- **原语命令面**：`add` → 按 label `query` → `set` → `play` 空 diff → `check` 回报告；
  拒绝（未知 label、坏 JSON、外来协议版本）是可读的值，且之后的命令仍被回答。
- **讲解计划**：未载入时 `plan.status` 报 `loaded:false`，`plan.next` 是 `no_plan`；
  载入两步计划后逐段前进、按光标回退、跳到末尾结算重放；被回退掉的对象按 label 查询是
  `no_such_label`（真的没了，不是旧 id）。
- **两个客户端，一棵树**：两个独立进程重放同一份按 label 寻址的命令日志，最终 `SemanticTree`
  逐字段相同。

这套检查不替代 `crates/rustim-app/tests/conformance.rs`（那份覆盖 17 组断言），也不替代
Python 版；三份加起来证明的是：同一份协议文档，可以在互不相识的三种语言里各自实现正确。
