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
//  1. Reply to every action, including a name this app has never heard
//     of. Silence and a slow handler look the same from the shepherd's
//     side, so an operator's typo costs them the whole action_timeout.
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
	"os"
	"strings"
	"time"

	"shep-examples-go-chatty/channel"
)

var levels = []string{"trace", "debug", "info", "warn", "error"}

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
		var n uintptr
		if _, err := fmt.Sscanf(fd, "%d", &n); err != nil {
			return nil, fmt.Errorf("SHEP_CHANNEL_FD is %q, not a number", fd)
		}
		return os.NewFile(n, "shep-channel"), nil
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
	if params == nil {
		return "triggers"
	}
	if name := strings.TrimSpace(*params); name != "" {
		return name
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
func parseLevel(params *string) string {
	if params == nil {
		return ""
	}
	words := strings.Fields(*params)
	if len(words) == 0 {
		return ""
	}
	for _, level := range levels {
		if words[0] == level {
			return level
		}
	}
	return ""
}

func main() {
	conn, err := openChannel()
	if err != nil {
		fmt.Fprintln(os.Stderr, "go-chatty:", err)
		os.Exit(1)
	}

	if stamp := os.Getenv("SHEP_CHANNEL_VERSION"); stamp != "" && stamp != channel.Version {
		fmt.Fprintf(os.Stderr, "go-chatty: shepherd speaks channel %s, this app speaks %s\n",
			stamp, channel.Version)
	}

	send := func(message channel.ChildMessage) {
		line, err := json.Marshal(message)
		if err != nil {
			fmt.Fprintln(os.Stderr, "go-chatty: could not encode a message:", err)
			return
		}
		if _, err := conn.Write(append(line, '\n')); err != nil {
			fmt.Fprintln(os.Stderr, "go-chatty: the shepherd went away:", err)
			os.Exit(1)
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
	lines := bufio.NewScanner(conn)
	for lines.Scan() {
		var message channel.ShepherdMessage
		if err := json.Unmarshal(lines.Bytes(), &message); err != nil {
			fmt.Fprintln(os.Stderr, "go-chatty: could not read a message:", err)
			continue
		}

		if message.Kind == channel.KindShutdown {
			fmt.Println("go-chatty: the shepherd asked us to stop")
			return
		}
		if message.Kind != channel.KindAction || message.Name == nil || message.ID == nil {
			continue
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
			if level := parseLevel(message.Params); level != "" {
				body = "log level is now " + level
			} else {
				body = "usage: level <" + strings.Join(levels, "|") + "> [key=value ...]"
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
	}

	// Scan stops on a read error and on a line past its buffer, and both
	// look like a clean end without this.
	if err := lines.Err(); err != nil {
		fmt.Fprintln(os.Stderr, "go-chatty: the channel ended badly:", err)
	}

	// The shepherd going away is not a reason to stop. shep-channel leaves a
	// Rust app running for the same reason: a channel is something an app
	// has, not what it is for, and a shepherd can be replaced under it.
	fmt.Println("go-chatty: the shepherd went away; still running")
	for {
		time.Sleep(time.Hour)
	}
}

func str(s string) *string { return &s }

func num(f float64) *float64 { return &f }
