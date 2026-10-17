#!/usr/bin/env node
// The third client: drive a real rustim binary from Node.
//
// `crates/rustim-app/tests/conformance.rs` is the exhaustive conformance
// suite, written in Rust against `rustim-client`; `sdks/python/conformance.py`
// asks the same question in a second language. This runner asks it a third
// time, sharing no code with either: **does a client written only from
// `驱动协议v1.md` get the same answers?** It is the executable form of the
// principle in `贾维斯集成方案.md` §7 — "换一个 AI 客户端做同样的事，引擎零改动".
//
// It checks the parts of the protocol every client must agree on:
//
//   1. the handshake names this build's whole vocabulary and both profiles;
//   2. add -> query -> set -> play return the documented reply shapes;
//   3. check answers a report, and a refusal is a value that does not stop the
//      feed;
//   4. the plan verbs answer `plan` / `diff` and repeat the lecture from its
//      own baseline;
//   5. **two independent processes replay the same command log and answer with
//      the same SemanticTree** — the "switch clients freely" acceptance in its
//      stdio form.
//
// Usage:
//
//   cargo build -p rustim-app
//   node sdks/typescript/conformance.mjs
//
// Exit codes: 0 all checks passed, 1 a check failed, 2 the question could not
// be asked (no binary). A failed check names the command, the expected shape
// and the reply that arrived, because a conformance failure is almost always a
// client and engine disagreeing about one field.
//
// The runner needs no package, no node_modules and no network: Node's standard
// library only, the same constraint `rustim-client` holds in Rust.

import { accessSync, constants, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parseArgs } from "node:util";

import { Client, ProtocolError } from "./rustim-client.mjs";

/** A check failed. Carries the sentence to print. */
class Failure extends Error {
  constructor(message) {
    super(message);
    this.name = "Failure";
  }
}

function check(condition, message) {
  if (!condition) throw new Failure(message);
}

function equal(actual, expected, what) {
  if (actual !== expected) {
    throw new Failure(
      `${what}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`,
    );
  }
}

function expectOk(reply, what) {
  if ("error" in reply) {
    throw new Failure(`${what} was refused: ${JSON.stringify(reply.error)}`);
  }
  return reply;
}

function diffIsEmpty(reply) {
  const diff = reply.diff;
  return (
    diff !== undefined &&
    diff.created.length === 0 &&
    diff.modified.length === 0 &&
    diff.removed.length === 0
  );
}

/**
 * Spawn one headless session with its own still path.
 *
 * The `--png` path is per session because two processes writing one file is a
 * race with no intended winner.
 */
function session(binary, directory, name) {
  return Client.spawn(binary, [
    "--script",
    "-",
    "--reply",
    "--png",
    join(directory, `${name}.png`),
  ]);
}

/** The commands a lesson is built from, answered by the real binary. */
async function primitiveSurface(binary, directory) {
  const client = session(binary, directory, "primitives");
  try {
    const caps = await client.hello();
    equal(caps.protocol, 1, "hello.protocol");
    check(
      caps.actions.length === 36,
      `hello.actions must name all 36 verbs, got ${caps.actions.length}`,
    );
    for (const verb of [
      "add",
      "query",
      "check",
      "plan.load",
      "plan.next",
      "plan.goto",
      "plan.status",
      "next_to",
      "arrange",
      "fit",
      "annotate",
      "highlight",
      "point_at",
    ]) {
      check(caps.actions.includes(verb), `hello.actions is missing ${JSON.stringify(verb)}`);
    }
    equal(caps.rates.length, 45, "hello.rates length");
    check(
      caps.profiles.includes("primitive") && caps.profiles.includes("teaching"),
      `hello.profiles must carry primitive and teaching, got ${JSON.stringify(caps.profiles)}`,
    );

    const added = expectOk(
      await client.send({
        action: "add",
        type: "circle",
        params: { radius: 1.0, label: "圆" },
      }),
      "add circle",
    );
    equal(added.diff.created.length, 1, "add created one id");
    const circle = added.diff.created[0];

    // A label is the address a model actually remembers; the engine resolves
    // it to the same id it handed out for the creation.
    const queried = expectOk(
      await client.send({ action: "query", target: "圆" }),
      "query by label",
    );
    equal(queried.tree.root, circle, "query's root is the addressed node");
    const names = queried.tree.nodes.map((node) => node.name);
    check(names.includes("圆"), `query's tree names the object: ${JSON.stringify(names)}`);

    const changed = expectOk(
      await client.send({
        action: "set",
        target: "圆",
        property: { name: "opacity", value: 0.25 },
      }),
      "set opacity",
    );
    equal(changed.diff.modified.length, 1, "set modified one node");
    equal(changed.diff.modified[0], circle, "set modified the circle");

    const played = expectOk(
      await client.send({
        action: "play",
        animation: { target: "圆", type: "fade_out", duration: 1.0 },
      }),
      "play",
    );
    check(
      diffIsEmpty(played),
      `play must answer a present-but-empty diff: ${JSON.stringify(played.diff)}`,
    );

    const checked = expectOk(await client.send({ action: "check" }), "check");
    check("report" in checked, "check must answer a report");
    equal(
      checked.report.findings.length,
      0,
      "an in-frame scene with a sane beat has no findings",
    );

    // A refusal is a value, and the feed keeps answering.
    let refused = await client.send({
      action: "set",
      target: "不存在",
      property: { name: "opacity", value: 0.5 },
    });
    equal(refused.error.error, "no_such_label", "an unknown label is no_such_label");
    refused = await client.sendRaw("this is not a command");
    equal(refused.error.error, "unparseable", "bad JSON is unparseable");
    refused = await client.send({ action: "hello", protocol: 2 });
    equal(refused.error.error, "out_of_range", "a foreign protocol version is out_of_range");
    equal(refused.error.property, "protocol", "the protocol refusal names its field");
    const stillThere = expectOk(
      await client.send({ action: "query", target: "圆" }),
      "query after refusals",
    );
    check("tree" in stillThere, "the feed keeps answering after a refusal");
  } finally {
    await client.close();
  }
}

/** A lecture is a document: pre-flighted, stepped and rewound over the wire. */
async function planNavigation(binary, directory) {
  const client = session(binary, directory, "plan");
  try {
    await client.hello();
    const idle = expectOk(await client.send({ action: "plan.status" }), "plan.status with no plan");
    equal(idle.plan.loaded, false, "plan.status.loaded before load");
    equal(idle.plan.steps, 0, "plan.status.steps before load");

    const refused = await client.send({ action: "plan.next" });
    equal(refused.error.error, "no_plan", "plan.next before load is no_plan");

    const loaded = expectOk(
      await client.send({
        action: "plan.load",
        plan: {
          label: "演示",
          steps: [
            {
              say: "先放一个方块",
              ops: [
                {
                  action: "add",
                  type: "square",
                  params: { width: 1.0, height: 1.0, label: "方块" },
                },
              ],
            },
            { say: "讲完了" },
          ],
        },
      }),
      "plan.load",
    );
    check(diffIsEmpty(loaded), "plan.load answers an empty diff");

    let status = expectOk(await client.send({ action: "plan.status" }), "plan.status");
    equal(status.plan.label, "演示", "plan.status.label");
    equal(status.plan.steps, 2, "plan.status.steps");
    equal(status.plan.cursor, 0, "plan.status.cursor");
    equal(status.plan.say, "先放一个方块", "plan.status.say is the next step's line");

    const stepped = expectOk(await client.send({ action: "plan.next" }), "plan.next");
    equal(stepped.diff.created.length, 1, "plan.next creates its step's op");
    const square = stepped.diff.created[0];
    expectOk(await client.send({ action: "query", target: "方块" }), "query the planned square");

    const rewound = expectOk(
      await client.send({ action: "plan.goto", step: 0 }),
      "plan.goto(0)",
    );
    equal(rewound.diff.removed.length, 1, "goto(0) removes one node");
    equal(rewound.diff.removed[0], square, "goto(0) removes what step 0 created");
    const gone = await client.send({ action: "query", target: "方块" });
    equal(gone.error.error, "no_such_label", "the rewound object is gone, not stale");

    const finished = expectOk(
      await client.send({ action: "plan.goto", step: 2 }),
      "plan.goto(2)",
    );
    equal(finished.diff.created.length, 1, "goto(2) replays the lecture settled");
    status = expectOk(await client.send({ action: "plan.status" }), "plan.status at the end");
    equal(status.plan.cursor, 2, "plan.status.cursor at the end");
    equal(status.plan.say, undefined, "the end has no next line");
  } finally {
    await client.close();
  }
}

/**
 * The command log both replay clients send, byte for byte. It is addressed by
 * label on purpose: a client that had to remember an id from a different
 * process would not be testing switching.
 */
const REPLAY_LOG = [
  {
    action: "add",
    type: "circle",
    params: { radius: 1.0, color: [100, 181, 246], label: "甲" },
  },
  {
    action: "add",
    type: "square",
    params: { width: 1.0, height: 1.0, position: [2.0, 0.0], label: "乙" },
  },
  {
    action: "set",
    target: "甲",
    property: { name: "opacity", value: 0.25 },
  },
  {
    action: "set",
    target: "乙",
    property: { name: "stroke_color", value: [239, 83, 80] },
  },
  {
    action: "play",
    animation: { target: "甲", type: "move_to", to: [1.0, 1.0], duration: 1.0 },
  },
  { action: "query" },
];

async function replay(binary, directory, name, log) {
  const client = session(binary, directory, name);
  try {
    await client.hello();
    let last = null;
    for (const command of log) {
      last = expectOk(await client.send(command), command.action);
    }
    return last.tree;
  } finally {
    await client.close();
  }
}

/** Two independent processes, one log, one tree — the switching acceptance. */
async function twoClients(binary, directory) {
  const first = await replay(binary, directory, "first", REPLAY_LOG);
  const second = await replay(binary, directory, "second", REPLAY_LOG);
  const a = JSON.stringify(first);
  const b = JSON.stringify(second);
  if (a !== b) {
    throw new Failure(
      `two clients replayed the same log onto different trees:\nfirst:  ${a}\nsecond: ${b}`,
    );
  }
  check(first.nodes.length > 1, "the replayed tree is not empty");
}

async function run(binary) {
  const directory = mkdtempSync(join(tmpdir(), "rustim-conformance-"));
  const checks = [
    ["primitive surface", primitiveSurface],
    ["plan navigation", planNavigation],
    ["two clients, one tree", twoClients],
  ];
  let failed = 0;
  try {
    for (const [name, scenario] of checks) {
      try {
        await scenario(binary, directory);
        console.log(`ok   ${name}`);
      } catch (error) {
        if (!(error instanceof Failure) && !(error instanceof ProtocolError)) throw error;
        failed += 1;
        console.log(`FAIL ${name}: ${error.message}`);
      }
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
  return failed === 0 ? 0 : 1;
}

function main() {
  const { values } = parseArgs({
    options: {
      binary: { type: "string", default: join("target", "debug", "rustim-app") },
    },
  });
  try {
    accessSync(values.binary, constants.X_OK);
  } catch {
    console.error(
      `cannot ask: no executable at ${values.binary} (run \`cargo build -p rustim-app\`)`,
    );
    return 2;
  }
  return run(values.binary);
}

process.exitCode = await main();
