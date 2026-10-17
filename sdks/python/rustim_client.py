"""A synchronous, dependency-free client for rustim's stdio command protocol.

This module is the Python half of the protocol's framing rules and nothing
else.  It turns a command into one JSON line, reads the one reply line back,
and checks the two promises the wire makes:

* **one reply per command line** (a blank command line is a skip and produces
  no reply);
* **exactly one of `diff` / `tree` / `report` / `plan` / `capabilities` /
  `error`** is present in a reply, and the presence is the discriminant.

The Rust implementation of the same rules is `crates/rustim-client`; the
conformance suite in `crates/rustim-app/tests/conformance.rs` drives the real
binary through it.  This module exists so the same claim can be made from a
second language — see `贾维斯集成方案.md` §7: "换一个 AI 客户端做同样的事，
引擎零改动".

Usage::

    # Run the interpreter with -I; add this directory to sys.path explicitly.
    from rustim_client import Client

    with Client.spawn("target/debug/rustim-app",
                      ["--script", "-", "--reply", "--png", "/tmp/out.png"]) as client:
        caps = client.hello()
        reply = client.send({"action": "add", "type": "circle",
                             "params": {"radius": 1.0, "label": "圆"}})
        created = reply["diff"]["created"]

The client is deliberately small: no engine logic, no rendering, no platform
code.  It spawns a process, writes lines, reads lines, and refuses to pretend
a malformed stream is a reply.
"""

import json
import subprocess

#: The six reply presences, in the protocol's declaration order.  Exactly one
#: is present in every reply.
PRESENCES = ("diff", "tree", "report", "plan", "capabilities", "error")


class ProtocolError(RuntimeError):
    """The bytes on the wire do not obey the protocol's framing rules.

    Raised for a reply that is not JSON, one that carries zero or two
    presences, a `seq` that does not match the physical line, or a stream
    that closes before answering.  A refusal *from the engine* is not this:
    it arrives as a well-formed reply with an `error` field and is returned
    to the caller.
    """


class Client(object):
    """A rustim process driven over its stdin/stdout.

    The child's stdout is the protocol.  Its stderr is inherited, because the
    engine routes every human-facing line there and a driver that swallowed
    it would hide the reason a run failed.
    """

    def __init__(self, process):
        self._process = process
        self._stdin = process.stdin
        self._stdout = process.stdout
        #: Physical lines written so far, which is the `seq` the next reply
        #: must carry.  Blank lines count, exactly as the engine counts them.
        self._lines = 0

    @classmethod
    def spawn(cls, program, args=()):
        """Spawn `program` with `args`, connecting the protocol to its stdio.

        The caller owns the command line: this class does not know that
        `--script - --reply` is what makes a rustim binary answer.  A
        `--script -` host runs headless only alongside `--png`, `--inspect`
        or a video mode, so pass one of those.
        """
        process = subprocess.Popen(
            [str(program)] + list(args),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            # Inherit stderr: informational lines and refusal summaries are
            # written there, and capturing without reading can block a child.
            universal_newlines=True,
        )
        return cls(process)

    def write_raw(self, line):
        """Write one line and flush it, **without** reading a reply.

        A blank line is legal here: it is the protocol's skip and produces no
        reply.
        """
        if self._stdin is None:
            raise ValueError("the client is already closed")
        self._stdin.write(line + "\n")
        self._stdin.flush()
        self._lines += 1

    def send_raw(self, line):
        """Send one command line and return its reply as a dict.

        A blank line is refused: it has no reply, and this method's contract
        is one in, one out.
        """
        if not line.strip():
            raise ValueError("a blank line is a skip and has no reply; use write_raw")
        self.write_raw(line)
        expected = self._lines
        reply = self._read_reply()
        if reply.get("seq") != expected:
            raise ProtocolError(
                "reply seq %r does not match line %d" % (reply.get("seq"), expected)
            )
        return reply

    def send(self, command):
        """Send a typed command (a dict) and return its reply."""
        return self.send_raw(json.dumps(command, ensure_ascii=False))

    def hello(self):
        """The handshake: this build's capabilities.

        A refusal (for example a foreign protocol version) is raised as
        [`ProtocolError`] carrying the engine's own words, because the caller
        is not in a position to do anything but report them.
        """
        reply = self.send({"action": "hello", "protocol": 1})
        if "error" in reply:
            raise ProtocolError("hello refused: %s" % json.dumps(reply["error"]))
        if "capabilities" not in reply:
            raise ProtocolError("hello was answered without capabilities")
        return reply["capabilities"]

    def close(self):
        """Close the protocol stream and wait for the process.

        Dropping the write end is what tells a `--script -` host that the
        script has ended.  Returns the exit status (an int); calling close
        twice is not possible because the stream is cleared.
        """
        if self._stdin is not None:
            self._stdin.close()
            self._stdin = None
        return self._process.wait()

    def kill(self):
        """Terminate the child without waiting for a clean exit.

        For a runner that failed mid-scenario: leaving a `--script -` child
        reading stdin would hang the process that spawned it.
        """
        if self._stdin is not None:
            self._stdin.close()
            self._stdin = None
        self._process.kill()
        self._process.wait()

    def _read_reply(self):
        while True:
            line = self._stdout.readline()
            if not line:
                raise ProtocolError("the engine closed the stream before replying")
            text = line.strip()
            if not text:
                # The engine never writes a blank line; a transport that adds
                # one must not turn every following reply into a parse error.
                continue
            try:
                reply = json.loads(text)
            except ValueError:
                raise ProtocolError("not a reply: %s" % text)
            if not isinstance(reply, dict):
                raise ProtocolError("not a reply object: %s" % text)
            present = [name for name in PRESENCES if name in reply]
            if len(present) != 1:
                raise ProtocolError(
                    "a reply must carry exactly one of %s: %s"
                    % ("/".join(PRESENCES), text)
                )
            return reply

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        if exc_type is None:
            status = self.close()
            if status != 0:
                raise ProtocolError("the engine exited with status %d" % status)
        else:
            self.kill()
        return False
