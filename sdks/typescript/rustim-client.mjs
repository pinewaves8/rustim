// A synchronous-looking, dependency-free client for rustim's stdio protocol.
//
// This module is the JavaScript half of the protocol's framing rules and
// nothing else. It turns a command into one JSON line, reads the one reply
// line back, and checks the three promises the wire makes:
//
//   * one reply per command line (a blank command line is a skip and produces
//     no reply);
//   * `seq` is the 1-based physical line number, blank lines included;
//   * exactly one of `diff` / `tree` / `report` / `plan` / `capabilities` /
//     `error` is present, and the presence is the discriminant.
//
// The Rust implementation of the same rules is `crates/rustim-client`; the
// conformance suite in `crates/rustim-app/tests/conformance.rs` drives the
// real binary through it. This module exists so the same claim can be made
// from a third language — see `贾维斯集成方案.md` §7: "换一个 AI 客户端做
// 同样的事，引擎零改动".
//
// Usage:
//
//   import { Client } from "./rustim-client.mjs";
//
//   const client = Client.spawn("target/debug/rustim-app",
//     ["--script", "-", "--reply", "--png", "/tmp/out.png"]);
//   const caps = await client.hello();
//   const reply = await client.send({ action: "add", type: "circle",
//     params: { radius: 1.0, label: "圆" } });
//   const created = reply.diff.created;
//   await client.close();
//
// The client is deliberately small: no engine logic, no rendering, no
// platform code. It spawns a process, writes lines, reads lines, and refuses
// to pretend a malformed stream is a reply.

import { spawn } from "node:child_process";
import { once } from "node:events";
import { createInterface } from "node:readline";

//: The six reply presences, in the protocol's declaration order. Exactly one
//: is present in every reply.
export const PRESENCES = Object.freeze([
  "diff",
  "tree",
  "report",
  "plan",
  "capabilities",
  "error",
]);

/**
 * The bytes on the wire do not obey the protocol's framing rules.
 *
 * Thrown for a reply that is not JSON, one that carries zero or two
 * presences, a `seq` that does not match the physical line, or a stream that
 * closes before answering. A refusal *from the engine* is not this: it
 * arrives as a well-formed reply with an `error` field and is returned to the
 * caller.
 */
export class ProtocolError extends Error {
  constructor(message) {
    super(message);
    this.name = "ProtocolError";
  }
}

/** An asynchronous line queue over a readable stream. */
class LineReader {
  #lines = [];
  #waiters = [];
  #ended = false;

  constructor(stream) {
    const reader = createInterface({ input: stream, crlfDelay: Infinity });
    reader.on("line", (line) => this.#deliver(line));
    reader.on("close", () => {
      this.#ended = true;
      this.#wake();
    });
    stream.on("error", () => {
      this.#ended = true;
      this.#wake();
    });
  }

  #deliver(line) {
    const waiter = this.#waiters.shift();
    if (waiter) waiter(line);
    else this.#lines.push(line);
  }

  #wake() {
    while (this.#waiters.length > 0) this.#waiters.shift()(null);
  }

  /** Resolves to the next line, or `null` once the stream has ended. */
  next() {
    if (this.#lines.length > 0) return Promise.resolve(this.#lines.shift());
    if (this.#ended) return Promise.resolve(null);
    return new Promise((resolve) => this.#waiters.push(resolve));
  }
}

/**
 * A rustim process driven over its stdin/stdout.
 *
 * The child's stdout is the protocol. Its stderr is inherited, because the
 * engine routes every human-facing line there and a driver that swallowed it
 * would hide the reason a run failed.
 */
export class Client {
  #stdin;
  #reader;
  #lines = 0;

  constructor(child) {
    this.child = child;
    this.#stdin = child.stdin;
    this.#reader = new LineReader(child.stdout);
  }

  /**
   * Spawn `program` with `args`, connecting the protocol to its stdio.
   *
   * The caller owns the command line: this class does not know that
   * `--script - --reply` is what makes a rustim binary answer. A `--script -`
   * host runs headless only alongside `--png`, `--inspect` or a video mode,
   * so pass one of those.
   */
  static spawn(program, args = []) {
    const child = spawn(String(program), args.map(String), {
      stdio: ["pipe", "pipe", "inherit"],
    });
    return new Client(child);
  }

  /**
   * The physical lines written so far, which is the `seq` the next reply must
   * carry. Blank lines count, exactly as the engine counts them.
   */
  get lines() {
    return this.#lines;
  }

  /**
   * Write one line and flush it, **without** reading a reply.
   *
   * A blank line is legal here: it is the protocol's skip and produces no
   * reply.
   */
  writeRaw(line) {
    if (this.#stdin === null) throw new Error("the client is already closed");
    this.#stdin.write(line + "\n");
    this.#lines += 1;
  }

  /**
   * Send one command line and return its reply.
   *
   * A blank line is refused: it has no reply, and this method's contract is
   * one in, one out.
   */
  async sendRaw(line) {
    if (line.trim() === "") {
      throw new Error("a blank line is a skip and has no reply; use writeRaw");
    }
    this.writeRaw(line);
    const expected = this.#lines;
    const reply = await this.#readReply();
    if (reply.seq !== expected) {
      throw new ProtocolError(
        `reply seq ${JSON.stringify(reply.seq)} does not match line ${expected}`,
      );
    }
    return reply;
  }

  /** Send a typed command (an object) and return its reply. */
  async send(command) {
    return this.sendRaw(JSON.stringify(command));
  }

  /**
   * The handshake: this build's capabilities.
   *
   * A refusal (for example a foreign protocol version) is thrown as a
   * `ProtocolError` carrying the engine's own words, because the caller is not
   * in a position to do anything but report them.
   */
  async hello() {
    const reply = await this.send({ action: "hello", protocol: 1 });
    if ("error" in reply) {
      throw new ProtocolError(`hello refused: ${JSON.stringify(reply.error)}`);
    }
    if (!("capabilities" in reply)) {
      throw new ProtocolError("hello was answered without capabilities");
    }
    return reply.capabilities;
  }

  /**
   * Close the protocol stream and wait for the process.
   *
   * Dropping the write end is what tells a `--script -` host that the script
   * has ended. Resolves to the exit status (a number); calling close twice is
   * not possible because the stream is cleared.
   */
  async close() {
    if (this.#stdin !== null) {
      this.#stdin.end();
      this.#stdin = null;
    }
    const [code] = await once(this.child, "exit");
    return code;
  }

  /**
   * Terminate the child without waiting for a clean exit.
   *
   * For a runner that failed mid-scenario: leaving a `--script -` child
   * reading stdin would hang the process that spawned it.
   */
  async kill() {
    if (this.#stdin !== null) {
      this.#stdin.end();
      this.#stdin = null;
    }
    this.child.kill();
    await once(this.child, "exit").catch(() => {});
  }

  async #readReply() {
    for (;;) {
      const line = await this.#reader.next();
      if (line === null) {
        throw new ProtocolError("the engine closed the stream before replying");
      }
      const text = line.trim();
      if (text === "") {
        // The engine never writes a blank line; a transport that adds one must
        // not turn every following reply into a parse error.
        continue;
      }
      let reply;
      try {
        reply = JSON.parse(text);
      } catch {
        throw new ProtocolError(`not a reply: ${text}`);
      }
      if (reply === null || typeof reply !== "object" || Array.isArray(reply)) {
        throw new ProtocolError(`not a reply object: ${text}`);
      }
      const present = PRESENCES.filter((name) =>
        Object.prototype.hasOwnProperty.call(reply, name),
      );
      if (present.length !== 1) {
        throw new ProtocolError(
          `a reply must carry exactly one of ${PRESENCES.join("/")}: ${text}`,
        );
      }
      return reply;
    }
  }
}
