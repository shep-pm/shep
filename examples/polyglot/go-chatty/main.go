// Command go-chatty speaks the shepherd channel: readiness, a metric, and
// custom actions.
//
// The contract is docs/shepherd-channel.md, and this frames it by hand.
// A real Go app should depend on github.com/shep-pm/shep-go/channel
// instead, which does all of this and owns its own goroutines. This
// example takes no dependency so `go build` needs nothing but the
// toolchain, which leaves the wire visible for anyone porting it to a
// language with no library at all.
//
// The two message shapes come from channel/wire.go, a verbatim copy of
// crates/shep-channel/wire/channel.go, which shep generates from the Rust
// enums. build.sh refuses to build if the copy has drifted. Every
// optional field there is a pointer because Go's omitempty on a plain
// value drops a metric of zero and an id of zero.
//
// Three things a hand-roll has to get right, all of them below:
//
//  1. Reply to every action this app can answer, including one whose
//     name it has never heard of. Silence and a slow handler look the
//     same from the shepherd's side, so an operator's typo costs them
//     the whole action_timeout. A frame carrying no name or no id is not
//     answerable: there is nothing to name and nowhere to send it.
//  2. Echo the action's id on the reply. Without it shep matches replies
//     by name and by order, which goes wrong once two of one name are
//     outstanding.
//  3. Own the grammar of params. It is one opaque string that shep never
//     splits. See parseLevel.
//
// Usage: go-chatty
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"
	"time"
	"unicode"

	"shep-examples-go-chatty/channel"
)

var levels = []string{"trace", "debug", "info", "warn", "error"}

// What an unparsable level gets back. Built from levels rather than
// spelled out, so adding one cannot leave the message listing the old set.
// Says the rest is dropped rather than inviting arguments it does not read.
var usage = "usage: level <" + strings.Join(levels, "|") + "> [rest is ignored]"

// openChannel returns the one file this app reads and writes, or an error
// naming what would have opened one.
//
// Exactly one of the two variables is ever set, so branch on which one is
// present rather than on the platform. Windows gets a named pipe path to
// open; unix gets a descriptor it already holds.
func openChannel() (*os.File, error) {
	if pipe := os.Getenv("SHEP_CHANNEL_PIPE"); pipe != "" {
		return os.OpenFile(pipe, os.O_RDWR, 0)
	}
	if fd := os.Getenv("SHEP_CHANNEL_FD"); fd != "" {
		// Not Sscanf: it stops at the first non-digit, so "3abc" parses as
		// 3 and reports no error. ParseUint refuses the whole string.
		n, err := strconv.ParseUint(fd, 10, 64)
		if err != nil {
			return nil, fmt.Errorf("SHEP_CHANNEL_FD is %q, not a descriptor number", fd)
		}
		// NewFile wraps any number at all, so a descriptor that is not
		// open stays invisible until the first write fails, by which
		// point this app has already logged that it is ready.
		f := os.NewFile(uintptr(n), "shep-channel")
		if _, err := f.Stat(); err != nil {
			// NewFile hands ownership to the caller, so the refusing path
			// owes it a Close as much as the succeeding one does. This
			// example exits straight after, but it gets copied into code
			// that will not.
			// Discarded on purpose: the descriptor is already being refused,
			// and a Close error on it tells the operator nothing they can act
			// on. Written as `_ =` so the next reader sees a decision.
			_ = f.Close()
			return nil, fmt.Errorf("SHEP_CHANNEL_FD is %q, which is not an open descriptor", fd)
		}
		return f, nil
	}
	return nil, fmt.Errorf("no shepherd channel. Set channel = true on this app " +
		"in the Flockfile, or wait_ready, or shutdown_with_message")
}

// metricName names the metric one `metric` action should send.
//
// params reaches an app exactly as the operator typed it, so an empty or
// blank one is ordinary rather than a mistake. Both fall back, since a
// metric named "" is worse on the bus than no custom name at all.
func metricName(params *string) string {
	if params != nil {
		if name := strings.TrimSpace(*params); name != "" {
			return name
		}
	}
	return "triggers"
}

// parseLevel reads a log level out of one action's params, in this app's
// own grammar.
//
// shep passes whatever the operator typed as a single string and never
// looks inside it, so every app decides how its own actions are spelled.
// This one splits on whitespace and reads the first word, which means a
// level can never contain a space. An app needing one would put JSON in
// this string instead.
// The remainder comes back with the level so the reply can say it was
// dropped, because an app that silently ignores half of what it was handed
// is the thing this action exists to warn about.
func parseLevel(params *string) (level, rest string) {
	if params == nil {
		return "", ""
	}
	text := strings.TrimSpace(*params)
	first, rest := text, ""
	if i := strings.IndexFunc(text, unicode.IsSpace); i >= 0 {
		// Sliced, not Fields plus Join: rejoining would report a tab as a
		// space, and the whole point is saying what was actually dropped.
		first, rest = text[:i], strings.TrimSpace(text[i:])
	}
	for _, known := range levels {
		if first == known {
			return known, rest
		}
	}
	return "", ""
}

func main() {
	conn, err := openChannel()
	if err != nil {
		fmt.Fprintln(os.Stderr, "go-chatty:", err)
		os.Exit(1)
	}

	// Warn and carry on, unlike the missing-channel case above, which exits.
	// The contract asks an app to notice a wire it has never seen and say
	// so, not to refuse one: a later version may still carry these messages.
	if stamp := os.Getenv("SHEP_CHANNEL_VERSION"); stamp != "" && stamp != channel.Version {
		fmt.Fprintf(os.Stderr, "go-chatty: shepherd speaks channel %s, this app speaks %s\n",
			stamp, channel.Version)
	}

	// A failed write means the same thing a closed channel does, so it gets
	// the same answer: say so once and carry on. The read loop below ends on
	// its own next pass. Reporting every failure would bury the first one.
	gone := false
	send := func(message channel.ChildMessage) {
		line, err := json.Marshal(message)
		if err != nil {
			fmt.Fprintln(os.Stderr, "go-chatty: could not encode a message:", err)
			return
		}
		if _, err := conn.Write(append(line, '\n')); err != nil && !gone {
			gone = true
			fmt.Fprintln(os.Stderr, "go-chatty: could not write to the shepherd:", err)
		}
	}

	started := time.Now()
	send(channel.ChildMessage{Kind: channel.KindReady})
	send(channel.ChildMessage{Kind: channel.KindMetric, Name: str("starts"), Value: num(1)})
	fmt.Printf("go-chatty pid=%d ready on the shepherd channel\n", os.Getpid())

	// One goroutine reads and writes, which is what keeps the Windows arm
	// safe: a blocking read parked on a pipe handle would hold it against
	// this app's own writes. A metrics ticker is the usual way to end up
	// with two, so this app emits samples from the loop instead.
	samples := 0
	// A Reader, not a Scanner. Scan caps a line at 64KB by default and then
	// ends the loop, which looks exactly like a clean end of stream; raising
	// that cap only moves the number. The contract sets no length, and
	// shep-channel and the other two examples impose none either.
	lines := bufio.NewReader(conn)

	// Returns true when the shepherd asked this app to stop.
	handle := func(line string) bool {
		var message channel.ShepherdMessage
		if err := json.Unmarshal([]byte(line), &message); err != nil {
			fmt.Fprintln(os.Stderr, "go-chatty: could not read a message:", err)
			return false
		}

		if message.Kind == channel.KindShutdown {
			fmt.Println("go-chatty: the shepherd asked us to stop")
			return true
		}
		if message.Kind != channel.KindAction {
			return false
		}
		if message.Name == nil || message.ID == nil {
			fmt.Fprintln(os.Stderr, "go-chatty: ignoring an action with no name or no id")
			return false
		}

		name := *message.Name
		var body string
		switch {
		case name == "ping":
			body = fmt.Sprintf("pong from go pid=%d, up %.1fs", os.Getpid(), time.Since(started).Seconds())
		case name == "metric":
			samples++
			metric := metricName(message.Params)
			send(channel.ChildMessage{
				Kind:  channel.KindMetric,
				Name:  str(metric),
				Value: num(float64(samples)),
			})
			body = fmt.Sprintf("sent %s=%d", metric, samples)
		case name == "level":
			switch level, rest := parseLevel(message.Params); {
			case level == "":
				body = usage
			case rest == "":
				body = "log level is now " + level
			default:
				body = fmt.Sprintf("log level is now %s, ignored %q", level, rest)
			}
		default:
			body = "unknown action: " + name
		}

		send(channel.ChildMessage{
			Kind:   channel.KindActionReply,
			Action: str(name),
			Body:   str(body),
			ID:     message.ID,
		})
		return false
	}

	for {
		// ReadString hands back what it read alongside io.EOF when the last
		// line has no newline, so the line is handled before the error is.
		// A shepherd killed mid-write ends exactly that way, and the message
		// most likely to be sitting there is the shutdown.
		line, err := lines.ReadString('\n')
		if line != "" && handle(line) {
			return
		}
		if err != nil {
			if err != io.EOF {
				fmt.Fprintln(os.Stderr, "go-chatty: could not read from the shepherd:", err)
			}
			break
		}
	}

	// The shepherd going away is not a reason to stop. shep-channel leaves a
	// Rust app running for the same reason: a channel is something an app
	// has, not what it is for, and a shepherd can be replaced under it.
	fmt.Println("go-chatty: the shepherd went away; still running")
	// Not select{}, which is the usual way to block forever: this app runs on
	// one goroutine on purpose, so select{} is the only goroutine asleep and
	// Go's deadlock detector ends the process with exit 2.
	for {
		time.Sleep(time.Hour)
	}
}

func str(s string) *string { return &s }

func num(f float64) *float64 { return &f }
