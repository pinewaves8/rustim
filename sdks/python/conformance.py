#!/usr/bin/env python3
"""The second client: drive a real rustim binary from Python.

`crates/rustim-app/tests/conformance.rs` is the exhaustive conformance suite,
written in Rust against `rustim-client`.  This runner asks the question that
suite cannot ask from inside one language: **does a client that shares no code
with the engine get the same answers?**  It is the executable form of the
principle in `贾维斯集成方案.md` §7 — "换一个 AI 客户端做同样的事，引擎零改动".

It checks the parts of the protocol every client must agree on:

1. the handshake names this build's whole vocabulary and both profiles;
2. `add` → `query` → `set` → `play` return the documented reply shapes;
3. `check` answers a report, and a refusal is a value that does not stop the
   feed;
4. the plan verbs answer `plan` / `diff` and repeat the lecture from its own
   baseline;
5. **two independent processes replay the same command log and answer with
   the same `SemanticTree`** — the "switch clients freely" acceptance in its
   stdio form.

Usage::

    cargo build -p rustim-app
    python3 -I sdks/python/conformance.py

Exit codes: 0 all checks passed, 1 a check failed, 2 the question could not
be asked (no binary).  A failed check names the command, the expected shape
and the reply that arrived, because a conformance failure is almost always a
client and engine disagreeing about one field.

The runner needs no package, no virtualenv and no network: Python's standard
library only, the same constraint `crates/rustim-client` holds in Rust.
"""

import argparse
import json
import os
import shutil
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from rustim_client import Client, ProtocolError  # noqa: E402


class Failure(Exception):
    """A check failed.  Carries the sentence to print."""


def check(condition, message):
    if not condition:
        raise Failure(message)


def equal(actual, expected, what):
    if actual != expected:
        raise Failure("%s: expected %r, got %r" % (what, expected, actual))


def reply_error(reply):
    return reply.get("error")


def expect_ok(reply, what):
    if "error" in reply:
        raise Failure("%s was refused: %s" % (what, json.dumps(reply["error"])))
    return reply


def diff_is_empty(reply):
    return reply.get("diff") == {"created": [], "modified": [], "removed": []}


def session(binary, directory, name):
    """Spawn one headless session with its own still path.

    The `--png` path is per session because two processes writing one file is
    a race with no intended winner.
    """
    return Client.spawn(
        binary,
        ["--script", "-", "--reply", "--png", os.path.join(directory, name + ".png")],
    )


def primitive_surface(binary, directory):
    """The commands a lesson is built from, answered by the real binary."""
    client = session(binary, directory, "primitives")
    try:
        caps = client.hello()
        equal(caps["protocol"], 1, "hello.protocol")
        check(len(caps["actions"]) == 36,
              "hello.actions must name all 36 verbs, got %d" % len(caps["actions"]))
        for verb in ("add", "query", "check", "plan.load", "plan.next", "plan.goto",
                     "plan.status", "next_to", "arrange", "fit", "annotate",
                     "highlight", "point_at"):
            check(verb in caps["actions"], "hello.actions is missing %r" % verb)
        equal(len(caps["rates"]), 45, "hello.rates length")
        check("primitive" in caps["profiles"] and "teaching" in caps["profiles"],
              "hello.profiles must carry primitive and teaching, got %r"
              % (caps["profiles"],))

        added = expect_ok(
            client.send({"action": "add", "type": "circle",
                         "params": {"radius": 1.0, "label": "圆"}}),
            "add circle",
        )
        equal(len(added["diff"]["created"]), 1, "add created one id")
        circle = added["diff"]["created"][0]

        # A label is the address a model actually remembers; the engine
        # resolves it to the same id it handed out for the creation.
        queried = expect_ok(client.send({"action": "query", "target": "圆"}), "query by label")
        equal(queried["tree"]["root"], circle, "query's root is the addressed node")
        names = [node.get("name") for node in queried["tree"]["nodes"]]
        check("圆" in names, "query's tree names the object: %r" % (names,))

        changed = expect_ok(
            client.send({"action": "set", "target": "圆",
                         "property": {"name": "opacity", "value": 0.25}}),
            "set opacity",
        )
        equal(changed["diff"]["modified"], [circle], "set modified the circle")

        played = expect_ok(
            client.send({"action": "play",
                         "animation": {"target": "圆", "type": "fade_out", "duration": 1.0}}),
            "play",
        )
        check(diff_is_empty(played), "play must answer a present-but-empty diff: %r"
              % (played["diff"],))

        checked = expect_ok(client.send({"action": "check"}), "check")
        check("report" in checked, "check must answer a report")
        equal(checked["report"]["findings"], [], "an in-frame scene with a sane beat has no findings")

        # A refusal is a value, and the feed keeps answering.
        refused = client.send({"action": "set", "target": "不存在",
                               "property": {"name": "opacity", "value": 0.5}})
        equal(reply_error(refused).get("error"), "no_such_label",
              "an unknown label is no_such_label")
        refused = client.send_raw("this is not a command")
        equal(reply_error(refused).get("error"), "unparseable", "bad JSON is unparseable")
        refused = client.send({"action": "hello", "protocol": 2})
        error = reply_error(refused)
        equal(error.get("error"), "out_of_range", "a foreign protocol version is out_of_range")
        equal(error.get("property"), "protocol", "the protocol refusal names its field")
        still_there = expect_ok(client.send({"action": "query", "target": "圆"}),
                                "query after refusals")
        check("tree" in still_there, "the feed keeps answering after a refusal")
    finally:
        client.close()


def plan_navigation(binary, directory):
    """A lecture is a document: pre-flighted, stepped and rewound over the wire."""
    client = session(binary, directory, "plan")
    try:
        client.hello()
        idle = expect_ok(client.send({"action": "plan.status"}), "plan.status with no plan")
        equal(idle["plan"]["loaded"], False, "plan.status.loaded before load")
        equal(idle["plan"]["steps"], 0, "plan.status.steps before load")

        refused = client.send({"action": "plan.next"})
        equal(reply_error(refused).get("error"), "no_plan", "plan.next before load is no_plan")

        loaded = expect_ok(
            client.send({
                "action": "plan.load",
                "plan": {
                    "label": "演示",
                    "steps": [
                        {"say": "先放一个方块",
                         "ops": [{"action": "add", "type": "square",
                                  "params": {"width": 1.0, "height": 1.0,
                                             "label": "方块"}}]},
                        {"say": "讲完了"},
                    ],
                },
            }),
            "plan.load",
        )
        check(diff_is_empty(loaded), "plan.load answers an empty diff")

        status = expect_ok(client.send({"action": "plan.status"}), "plan.status")
        equal(status["plan"]["label"], "演示", "plan.status.label")
        equal(status["plan"]["steps"], 2, "plan.status.steps")
        equal(status["plan"]["cursor"], 0, "plan.status.cursor")
        equal(status["plan"]["say"], "先放一个方块", "plan.status.say is the next step's line")

        stepped = expect_ok(client.send({"action": "plan.next"}), "plan.next")
        equal(len(stepped["diff"]["created"]), 1, "plan.next creates its step's op")
        square = stepped["diff"]["created"][0]
        expect_ok(client.send({"action": "query", "target": "方块"}), "query the planned square")

        rewound = expect_ok(client.send({"action": "plan.goto", "step": 0}), "plan.goto(0)")
        equal(rewound["diff"]["removed"], [square], "goto(0) removes what step 0 created")
        refused = client.send({"action": "query", "target": "方块"})
        equal(reply_error(refused).get("error"), "no_such_label",
              "the rewound object is gone, not stale")

        finished = expect_ok(client.send({"action": "plan.goto", "step": 2}), "plan.goto(2)")
        equal(len(finished["diff"]["created"]), 1, "goto(2) replays the lecture settled")
        status = expect_ok(client.send({"action": "plan.status"}), "plan.status at the end")
        equal(status["plan"]["cursor"], 2, "plan.status.cursor at the end")
        equal(status["plan"].get("say"), None, "the end has no next line")
    finally:
        client.close()


#: The command log both replay clients send, byte for byte.  It is addressed
#: by label on purpose: a client that had to remember an id from a different
#: process would not be testing switching.
REPLAY_LOG = [
    {"action": "add", "type": "circle",
     "params": {"radius": 1.0, "color": [100, 181, 246], "label": "甲"}},
    {"action": "add", "type": "square",
     "params": {"width": 1.0, "height": 1.0, "position": [2.0, 0.0], "label": "乙"}},
    {"action": "set", "target": "甲", "property": {"name": "opacity", "value": 0.25}},
    {"action": "set", "target": "乙",
     "property": {"name": "stroke_color", "value": [239, 83, 80]}},
    {"action": "play",
     "animation": {"target": "甲", "type": "move_to", "to": [1.0, 1.0], "duration": 1.0}},
    {"action": "query"},
]


def replay(binary, directory, name, log):
    client = session(binary, directory, name)
    try:
        client.hello()
        last = None
        for command in log:
            last = expect_ok(client.send(command), command["action"])
        return last["tree"]
    finally:
        client.close()


def two_clients(binary, directory):
    """Two independent processes, one log, one tree — the switching acceptance."""
    first = replay(binary, directory, "first", REPLAY_LOG)
    second = replay(binary, directory, "second", REPLAY_LOG)
    if first != second:
        raise Failure(
            "two clients replayed the same log onto different trees:\n"
            "first:  %s\nsecond: %s"
            % (json.dumps(first, sort_keys=True), json.dumps(second, sort_keys=True))
        )
    check(len(first["nodes"]) > 1, "the replayed tree is not empty")


def run(binary):
    directory = tempfile.mkdtemp(prefix="rustim-conformance-")
    checks = [
        ("primitive surface", lambda: primitive_surface(binary, directory)),
        ("plan navigation", lambda: plan_navigation(binary, directory)),
        ("two clients, one tree", lambda: two_clients(binary, directory)),
    ]
    failed = 0
    try:
        for name, scenario in checks:
            try:
                scenario()
            except (Failure, ProtocolError) as error:
                failed += 1
                print("FAIL %s: %s" % (name, error))
            else:
                print("ok   %s" % name)
    finally:
        shutil.rmtree(directory, ignore_errors=True)
    return 0 if failed == 0 else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--binary",
        default=os.path.join("target", "debug", "rustim-app"),
        help="the rustim-app binary to drive (default: target/debug/rustim-app)",
    )
    args = parser.parse_args(argv)
    if not os.path.isfile(args.binary) or not os.access(args.binary, os.X_OK):
        print("cannot ask: no executable at %s (run `cargo build -p rustim-app`)"
              % args.binary, file=sys.stderr)
        return 2
    return run(args.binary)


if __name__ == "__main__":
    sys.exit(main())
