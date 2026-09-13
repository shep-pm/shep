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
function openChannel() {
  const pipe = process.env.SHEP_CHANNEL_PIPE;
  if (pipe) {
    return net.connect({ path: pipe });
  }
  const fd = process.env.SHEP_CHANNEL_FD;
  if (fd) {
    return new net.Socket({ fd: Number(fd), readable: true, writable: true });
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

const channel = openChannel();
if (channel === null) {
  console.error(
    "node-chatty: no shepherd channel. Set channel = true on this app in " +
      "the Flockfile, or wait_ready, or shutdown_with_message.",
  );
  process.exit(1);
}

const stamp = process.env.SHEP_CHANNEL_VERSION;
if (stamp !== undefined && stamp !== "1") {
  console.error(`node-chatty: shepherd speaks channel ${stamp}, this app speaks 1`);
}

const send = (message) => channel.write(`${JSON.stringify(message)}\n`);

send({ kind: "ready" });
send({ kind: "metric", name: "starts", value: 1 });
console.log(`node-chatty pid=${process.pid} ready on the shepherd channel`);

// The reply body is what the operator reads back from shep trigger.
let samples = 0;
function handle(message) {
  if (message.kind === "shutdown") {
    console.log("node-chatty: the shepherd asked us to stop");
    process.exit(0);
  }
  if (message.kind !== "action") {
    return;
  }

  const { name, params, id } = message;
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
    if (line.trim() !== "") {
      handle(JSON.parse(line));
    }
  }
});
channel.on("close", () => process.exit(0));
