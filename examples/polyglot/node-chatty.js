#!/usr/bin/env node
// Speaks the shepherd channel: readiness, a metric, and custom actions.
//
// The contract is docs/shepherd-channel.md, and this frames it by hand
// because JavaScript has no shep client library. examples/src/bin/chatty.rs
// is the same app in Rust, where the shep-channel crate does this part.
//
// Three things a hand-roll has to get right, all of them below:
//
//   1. Reply to every action this app can answer, including one whose
//      name it has never heard of. Silence and a slow handler look the
//      same from the shepherd's side, so an operator's typo costs them
//      the whole action_timeout. A frame carrying no name or no id is
//      not answerable: nothing to name, nowhere to send it.
//   2. Echo the action's id on the reply. Without it shep matches replies
//      by name and by order, which goes wrong once two of one name are
//      outstanding.
//   3. Own the grammar of params. It is one opaque string that shep never
//      splits. See parseLevel.
//
// Usage: node-chatty.js

const net = require("node:net");

const LEVELS = ["trace", "debug", "info", "warn", "error"];
// What an unparsable level gets back. Built from LEVELS rather than spelled
// out, so adding one cannot leave the message listing the old set. Says the
// rest is dropped rather than inviting arguments this app does not read.
const USAGE = `usage: level <${LEVELS.join("|")}> [rest is ignored]`;
const started = process.hrtime.bigint();

// Exactly one of the two variables is ever set, so branch on which one is
// present rather than on the platform. Windows gets a named pipe path to
// connect to; unix gets a socket descriptor it already holds. Either way
// this is one duplex stream, and libuv drives the pipe with overlapped
// I/O, so a pending read here never blocks a write the way a parked
// ReadFile on a synchronous handle would.
// Returns the stream and whether it is still opening. A descriptor is
// already open; a named pipe connects asynchronously, so a bad path fails
// after this returns and has to be caught on the socket instead.
function openChannel() {
  const pipe = process.env.SHEP_CHANNEL_PIPE;
  if (pipe) {
    return { socket: net.connect({ path: pipe }), opening: true };
  }
  const fd = process.env.SHEP_CHANNEL_FD;
  if (fd) {
    // Number("abc") is NaN, and NaN reaches the Socket constructor as a
    // TypeError nothing here would catch. Refuse it where it can be named.
    const n = Number(fd);
    if (!Number.isInteger(n) || n < 0) {
      throw new Error(`SHEP_CHANNEL_FD is "${fd}", not a descriptor number`);
    }
    return { socket: new net.Socket({ fd: n, readable: true, writable: true }), opening: false };
  }
  return null;
}

// Names the metric one `metric` action should send. params reaches an app
// exactly as the operator typed it, so an empty or blank one is ordinary
// rather than a mistake. Both fall back, since a metric named "" is worse
// on the bus than no custom name at all.
function metricName(params) {
  return (params ?? "").trim() || "triggers";
}

// shep passes whatever the operator typed as a single string and never
// looks inside it, so every app decides how its own actions are spelled.
// This one splits on whitespace and reads the first word, which means a
// level can never contain a space. An app needing one would put JSON in
// this string instead.
// The remainder comes back with the level so the reply can say it was
// dropped, because an app that silently ignores half of what it was handed
// is the thing this action exists to warn about.
function parseLevel(params) {
  // Sliced at the first space, not split into words and rejoined: rejoining
  // would report a tab as a space, and the point is saying what was dropped.
  const text = (params ?? "").trim();
  const cut = text.search(/\s/);
  const level = cut === -1 ? text : text.slice(0, cut);
  if (!LEVELS.includes(level)) {
    return null;
  }
  return { level, rest: cut === -1 ? "" : text.slice(cut).trim() };
}

let opened;
try {
  opened = openChannel();
} catch (err) {
  // A refusal an operator can act on, rather than the stack trace an
  // uncaught throw at module scope would print.
  console.error(`node-chatty: ${err.message}`);
  process.exit(1);
}
const channel = opened === null ? null : opened.socket;
if (channel === null) {
  console.error(
    "node-chatty: no shepherd channel. Set channel = true on this app in " +
      "the Flockfile, or wait_ready, or shutdown_with_message.",
  );
  process.exit(1);
}

// Warn and carry on, unlike the missing-channel case above, which exits.
// The contract asks an app to notice a wire it has never seen and say so,
// not to refuse one: a later version may still carry these messages.
const stamp = process.env.SHEP_CHANNEL_VERSION;
if (stamp !== undefined && stamp !== "1") {
  console.error(`node-chatty: shepherd speaks channel ${stamp}, this app speaks 1`);
}

// A failure before the pipe is open is a channel that never existed, and an
// operator can fix that. A failure afterwards is the shepherd going away,
// which this app has already decided is not a reason to stop. Without the
// handler at all, either one ends the process looking like a bug here.
let live = !opened.opening;
channel.on("connect", () => {
  live = true;
});
channel.on("error", (err) => {
  if (!live) {
    console.error(`node-chatty: cannot open the shepherd channel: ${err.message}`);
    process.exit(1);
  }
  console.error(`node-chatty: the channel failed: ${err.message}`);
});

const send = (message) => channel.write(`${JSON.stringify(message)}\n`);

// Held until the pipe is actually connected. Announcing readiness into a
// socket that is still opening prints a ready line this app then dies
// after, which is worse than saying nothing.
function announce() {
  send({ kind: "ready" });
  send({ kind: "metric", name: "starts", value: 1 });
  console.log(`node-chatty pid=${process.pid} ready on the shepherd channel`);
}
if (opened.opening) {
  channel.once("connect", announce);
} else {
  announce();
}

// The reply body is what the operator reads back from shep trigger.
let samples = 0;
function handle(message) {
  // Parsing is not the same as being a message. A bare number, list, string
  // or null is all valid JSON and none of them is one of ours.
  if (message === null || typeof message !== "object" || Array.isArray(message)) {
    console.error("node-chatty: ignoring a frame that is not an object");
    return;
  }
  if (message.kind === "shutdown") {
    console.log("node-chatty: the shepherd asked us to stop");
    // Replies queued earlier in this same chunk are still in the stream's
    // buffer, and process.exit drops whatever has not reached the kernel.
    // end() flushes first, then the callback ends the process. Measured at
    // 100 batched actions: 92 of them arrived before this, 100 after.
    channel.end(() => process.exit(0));
    return;
  }
  if (message.kind !== "action") {
    return;
  }

  // Every action carries both, so one that does not is not something this
  // app can answer, and carries nowhere to send the answer.
  const { name, params, id } = message;
  // `== null` on purpose: it catches both undefined and null, and a reply
  // carrying "id": null is one shep cannot match to anything. The typed
  // examples refuse it without asking, since null is not a number there.
  if (typeof name !== "string" || id == null) {
    console.error("node-chatty: ignoring an action with no name or no id");
    return;
  }
  // params is a string or it is absent. A typed language gets this free:
  // serde and encoding/json both refuse a number here and reject the whole
  // frame, so hand-rolling is where the check has to be written out.
  if (params !== undefined && params !== null && typeof params !== "string") {
    console.error(`node-chatty: ignoring ${name}, its params is not a string`);
    return;
  }

  let body;
  if (name === "ping") {
    const up = Number(process.hrtime.bigint() - started) / 1e9;
    body = `pong from node pid=${process.pid}, up ${up.toFixed(1)}s`;
  } else if (name === "metric") {
    samples += 1;
    const metric = metricName(params);
    send({ kind: "metric", name: metric, value: samples });
    body = `sent ${metric}=${samples}`;
  } else if (name === "level") {
    const parsed = parseLevel(params);
    if (parsed === null) {
      body = USAGE;
    } else if (parsed.rest === "") {
      body = `log level is now ${parsed.level}`;
    } else {
      body = `log level is now ${parsed.level}, ignored ${JSON.stringify(parsed.rest)}`;
    }
  } else {
    body = `unknown action: ${name}`;
  }

  send({ kind: "action-reply", action: name, body, id });
}

// One message per line, so buffer until a newline arrives. A read can
// carry half a message or two whole ones.
let pending = "";
// Enough of a frame to recognise it, bounded because a frame has no length
// limit and a log line should not inherit one.
function snippet(line) {
  const text = line.length > 80 ? `${line.slice(0, 80)}...` : line;
  return JSON.stringify(text);
}

// The parsed frame, or undefined after reporting the bytes that would not
// parse. The shepherd does not write these, so a frame that will not parse
// means a wire this app has never seen. Both readers below want the same
// report, and writing it twice drifted once already. Only the parse is
// guarded, so a real bug in handle still surfaces as itself.
function parseFrame(line, what) {
  try {
    return JSON.parse(line);
  } catch (err) {
    console.error(`node-chatty: could not read ${what}: ${err.message}, in ${snippet(line)}`);
    return undefined;
  }
}

channel.setEncoding("utf8");
channel.on("data", (chunk) => {
  pending += chunk;
  let cut;
  while ((cut = pending.indexOf("\n")) !== -1) {
    const line = pending.slice(0, cut);
    pending = pending.slice(cut + 1);
    if (line.trim() === "") {
      continue;
    }
    // A frame that will not parse costs this one line, never the action
    // after it.
    const message = parseFrame(line, "a message");
    if (message === undefined) {
      continue;
    }
    handle(message);
  }
});
// The shepherd going away is not a reason to stop. shep-channel leaves a
// Rust app running for the same reason: a channel is something an app has,
// not what it is for, and a shepherd can be replaced under it. The timer
// is what keeps this event loop alive once the socket is its only work.
// 'end', not 'close': it fires as soon as the shepherd stops sending, while
// 'close' waits for this side to finish too and would hold a reply that the
// tail below still owes. Whatever is left in pending had no newline, which
// is how a shepherd killed mid-write ends, and the message most likely
// sitting there is the shutdown.
channel.on("end", () => {
  const tail = pending.trim();
  pending = "";
  if (tail !== "") {
    const message = parseFrame(tail, "the last message");
    if (message === undefined) {
      return;
    }
    handle(message);
  }
});
channel.on("close", () => {
  console.log("node-chatty: the shepherd went away; still running");
  setInterval(() => {}, 1 << 30);
});
