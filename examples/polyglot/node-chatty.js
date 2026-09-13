#!/usr/bin/env node
// Speaks the shepherd channel: readiness, a metric, and custom actions.
//
// The contract is docs/shepherd-channel.md, and this frames it by hand
// because JavaScript has no shep client library. examples/src/bin/chatty.rs
// is the same app in Rust, where the shep-channel crate does this part.
//
// Three things a hand-roll has to get right, all of them below:
//
//   1. Reply to every action, including a name this app has never heard
//      of. Silence and a slow handler look the same from the shepherd's
//      side, so an operator's typo costs them the whole action_timeout.
//   2. Echo the action's id on the reply. Without it shep matches replies
//      by name and by order, which goes wrong once two of one name are
//      outstanding.
//   3. Own the grammar of params. It is one opaque string that shep never
//      splits. See parseLevel.
//
// Usage: node-chatty.js

const net = require("node:net");

const LEVELS = ["trace", "debug", "info", "warn", "error"];
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
function parseLevel(params) {
  const level = (params ?? "").trim().split(/\s+/)[0];
  return LEVELS.includes(level) ? level : null;
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
    process.exit(0);
  }
  if (message.kind !== "action") {
    return;
  }

  // Every action carries both, so one that does not is not something this
  // app can answer, and carries nowhere to send the answer.
  const { name, params, id } = message;
  if (typeof name !== "string" || id === undefined) {
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
    const level = parseLevel(params);
    body = level
      ? `log level is now ${level}`
      : `usage: level <${LEVELS.join("|")}> [key=value ...]`;
  } else {
    body = `unknown action: ${name}`;
  }

  send({ kind: "action-reply", action: name, body, id });
}

// One message per line, so buffer until a newline arrives. A read can
// carry half a message or two whole ones.
let pending = "";
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
    let message;
    try {
      message = JSON.parse(line);
    } catch (err) {
      // The shepherd does not write these, so a frame that will not parse
      // means a wire this app has never seen. Say so and read the next one;
      // dying here would also drop the action after it. Only the parse is
      // guarded, so a real bug in handle still surfaces as itself.
      console.error(`node-chatty: could not read a message: ${err.message}`);
      continue;
    }
    handle(message);
  }
});
// The shepherd going away is not a reason to stop. shep-channel leaves a
// Rust app running for the same reason: a channel is something an app has,
// not what it is for, and a shepherd can be replaced under it. The timer
// is what keeps this event loop alive once the socket is its only work.
channel.on("close", () => {
  console.log("node-chatty: the shepherd went away; still running");
  setInterval(() => {}, 1 << 30);
});
