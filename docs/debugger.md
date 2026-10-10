# Debugger

Den debugs any program that speaks the protocol below: Den doesn't know its
language or its VM. The program listens on a TCP port; Den reaches it through
the agent, so a program on a server is debugged like a local one.

## The launch file

`.den/debug.json` in the workspace:

```json
{ "command": "sim -d --debugger-port ${port} ${file}" }
```

- `command`: a shell line run in the debugger's own terminal, a part of
  its tab, never among the terminals' tabs (its output stays there). `${file}` is the open file, relative to the workspace: there is one
  command, which debugs whatever is open, and the program decides what that
  means (a script, a test file, the server it is part of). With no file open
  `${file}` is empty, and the program decides what to debug without one (sim:
  the server). Without `command`,
  F5 attaches to a program already running.
- `port`: where the program listens, on the loopback of the agent's machine.
  4444 when missing. A command with `${port}` doesn't need it: Den asks the
  agent for a free port and puts it there, so every session has a port of its
  own and none reaches another's program (another window's, one left
  running).

- `targets`: where the command runs the program, e.g.
  `["ios", "android", "chrome"]`. The names are the project's; Den gives
  them no meaning. The debugger's toolbar shows the target picked, which a
  click changes; each workspace keeps its own, the first until one is
  picked. `${target}` in `command` and in the tests' `run` and `debug` is
  it, and empty without `targets`. `den debug target` prints it and the
  list; `den debug target <name>` picks one. Restart keeps the target the
  session started with.

```json
{
    "command": "scl -d --debugger-port ${port} --target=${target} ${file}",
    "targets": ["ios", "android", "chrome"]
}
```

F5 runs the command. If something already answers on `port`, it attaches to
it instead of starting the command again; never with `${port}`, which is
always started. If the command's terminal goes back to its shell before the
program listens, the program ended (it didn't compile, its port was taken):
Den says so instead of waiting. While the command runs, Den waits for it as
long as it takes (an app's build), and shows its last line. A command is never typed into a terminal that
still runs something: the program before gets a few seconds to end, and then
the terminal is closed with it and the command gets a new one. A program held before running
(`waiting` in `hello`) is released with `entry`, so it stops at its entry, as
Visual Studio's debugger does: where the program says (sim: the first line of
`main`, or of the function `-de` names). A program with a page (`page` in
`hello`) is a server: it is released without `entry`, so it runs. Stop
(Shift-F5) interrupts a program it started and leaves one it attached to
running; once the session has ended, it closes the debugger. Restart (Cmd-Shift-F5) starts the session again as it started: the
same file (not the one a stop opened since), or the same test. A program it
started frees its terminal and its port first, so the new session doesn't
attach to the one that is ending.

### The program's page

A server names its page in `hello` (`page`, e.g.
`"http://localhost:9092/platform/tenants"`), and Den opens it in the browser
once something a terminal of the workspace runs listens on its port. Only when
the launch started the program: attaching or Restart doesn't open it again.
Only URLs of localhost; on a server the port is forwarded over SSH. The program
says it, not the launch file, because one command runs many things (a script,
a server, an app on a phone simulator) and only the server has a page, on the
port it chose. On macOS, a Google Chrome tab already showing that server (the
same port on `localhost`, a subdomain of it or `127.0.0.1`, whatever its page)
comes to the front as it is instead; the first time, macOS asks whether Den
may control Chrome. Elsewhere, or without such a tab, the page opens in the
default browser.

A file from when there were several `configurations` starts the first one.

### Tests

With a `tests` section, every line that declares a test gets Run (▷) and
Debug (the bug) at its end. Run starts `run` in a terminal; Debug starts `debug` under the
debugger. `${file}` is the open file and `${test}` the name `match` captured
(its first group). `${port}` in `debug`, as in `command`, is a free port of
its own; without it, `port` (4444 when missing) is where `debug` listens.

```json
"tests": {
    "match": "^export function (test\\w*)\\(",
    "run": "sim test ${file} ${test} -x",
    "debug": "sim -d --debugger-port ${port} test ${file} ${test} -x -c 1"
}
```

## From a terminal

`den debug` does what the keys do, from a terminal of the workspace, and
prints JSON, so an agent can test with the debugger:

```sh
den debug break modules/billing/main.ts:120
den debug start modules/billing/main.ts   # F5 on that file
curl -s localhost:9092/api/billing/... &  # what reaches the breakpoint
den debug wait                            # until a VM stops: the state
den debug eval 'invoice.total'            # {"value": "120", "type": "int"}
den debug next                            # then wait again
den debug continue
```

`den debug restart` is Cmd-Shift-F5: it stops the session and starts it
again. `den debug state` is the session (`idle`, `connecting: …`, `connected`), the
target, the stopped VMs, the focused stop (file, line, frames, locals, exception), the
breakpoints and the last lines of the console. `wait [stop|connected|idle]
[<seconds>]` waits for that (30 seconds at most) and prints the state; a
session that fails ends the wait too, and the console says why. `den
--help` lists them all.

## Several programs: `den debug join`

`den debug join` runs several programs as one debug session: a server and
the page it serves in Chrome, or a server and an app.

```sh
den debug join --port P [--no-page] -- <command> [<args>...] -- <command> [<args>...] [-- ...]
```

A launch file that debugs a server and its page in Chrome (the Chrome
bridge, `den chrome`, is in [chrome.md](chrome.md)). The bridge opens the
page itself, so `--no-page` keeps Den from opening the server's too:

```json
{
    "command": "den debug join --port ${port} --no-page -- sim -d --debugger-port {port} ${file} -- den chrome --port {port} --url http://localhost:9092/"
}
```

- `{port}` in a command is a free port of its own, where that program
  listens for the debugger. Every command needs one. `${port}` is Den's,
  where join listens. A command can't contain `--`.
- The programs start in order, each once the one before listens and
  answered `hello`: an app that starts its own server when none runs finds
  the one before. There is no time limit (an app's build takes minutes);
  the output of each goes on showing meanwhile, its lines after its name.
  A program that ends before it listens ends join, with its exit and its
  last lines of stderr. Programs that report different `cwd` end it too.
- join serves protocol v1 on `127.0.0.1:P` to one client, as a program
  does: the first line must be `hello`, and a new connection replaces the
  one before.
- Every `vm`, `ref` and `globals` number a program sends becomes
  `n * 16 + i`, where `i` is the program's place (16 programs at most; 0
  stays 0). A request with `vm` or `ref` goes to that program, with its own
  number. Any other goes to every program and the answers are merged:
  `hello` (`waiting` if any is, `running` added up, `stopped` together, the
  first `page` unless `--no-page`), `threads` (the same), `setBreakpoints`
  (each line where any program could put it: a program keeps the
  breakpoints of files it doesn't load), `run`, `setExceptions` and
  `pause` (an error if any program failed), and any other command (the
  first program that answers it, e.g. `inspect`).
- A client's session is a connection to each program, opened at its
  `hello` and closed when it goes: each program then does what it does when
  its client goes (clears its breakpoints, resumes its VMs), and nothing
  holds a program while nobody debugs.
- The programs are one session: a program that ends is told in the console
  (an `output` event), and join ends the others and itself (closing the
  browser of `den chrome` ends the server with it, and the session). join
  also ends when interrupted (Stop interrupts the launch command): it then
  ends its programs, with SIGINT and, after 3 seconds, SIGKILL.

## Keys

| Key | |
|-----|---|
| F5 | Start, or continue the stopped VM |
| Shift-F5 | Stop |
| Cmd-Shift-F5 | Restart |
| F6 | Pause: the next VM that runs code stops |
| F9 | Toggle a breakpoint (also: click the gutter) |
| right-click the gutter | Condition, hit count or log message of a breakpoint |
| F10 / F11 / Shift-F11 | Step over / into / out |
| Ctrl-F10 | Run to the cursor |
| Ctrl-Shift-F10 | Make the cursor's line the next statement |
| right-click the code | While debugging: Toggle Breakpoint, Add Conditional Breakpoint, Add Logpoint; while stopped also Run to Cursor, Set Next Statement, Add to Watch and Evaluate in Console (the selection, or the name under the cursor) |
| Cmd-Shift-D | Show or hide the debugger's tab |

All of them can be changed in Settings.

Inspect, in the debugger's right-click menu, asks the program being debugged
to let the person pick a widget on its screen (`inspect`, below); the line
that made it opens in the editor, and Den's window comes to the front. Every
`reveal` the program sends does that; a stop doesn't raise the window. In
Chrome the pick is an Alt-click in the page (docs/chrome.md).

It is a tab after the terminals' (closing the tab hides it; F5 or Cmd-Shift-D
bring it back), with the debugger's state on it: yellow while stopped, green
while running. The toolbar at its top, and its parts under it: the call
stack and the variables, the watches and the breakpoints, in two rows of
two; then the launch's terminal (while there is one) and the console, which
always keeps a few lines. A part's header dragged near another part's edge
puts it beside it on that side: side by side, or one above the other, as
the terminals' splits do. Dropped in its middle, the two swap. Every line
between them is dragged to size them. A part's right-click menu hides it
and its neighbours take its space; Show, in the tab's right-click menu,
lists the parts that hide (all but the console). Where each part is, the
sizes and the hidden parts are kept with the layout (the debugging one has
its own), and `den where` lists the parts as placed (`tree`) and those
hidden.

A session has a layout of its own: while the workspace in front debugs, Den
uses the debugging layout (the side column closed and the debugger's tab in
front the first time), and puts the editing one back on Stop. A session that
ends on its own (the program ended, failed to start or went) leaves the
debugger in sight with what its console said, until Stop. A restart is one
session. What changes
while debugging stays for the next one (docs/guide.md, Layout).

When a VM stops, its line is marked and, unless it's at least five lines
inside the view, scrolled to the middle of the code; the values of the variables are written
at the end of the lines of its function, and hovering a name shows its value
in a card that opens like the variables view, already open one level. The
card waits a moment before it goes, or shows another name's, so the pointer
can reach it across other names; the wheel over it scrolls only the card.
Values that the last step changed are shown in another color. In the
variables view a double click edits a value; the console evaluates
expressions and assignments (`total = 5`) with the history on ↑ and ↓.

## Protocol v1

One JSON object per line over TCP.

- Request (Den → program): `{"id": 7, "cmd": "next", "vm": 3}`. Arguments are
  fields of the same object.
- Response: `{"id": 7, "ok": true, ...}` or `{"id": 7, "ok": false, "error": "message"}`.
- Event (program → Den): `{"event": "stopped", ...}`, without `id`.

One client at a time: a new connection replaces the previous one. When the
client goes away, its breakpoints are cleared and every stopped VM resumes, so
a server is never left frozen by a closed debugger.

### Model

- **VM**: a thread of execution with a numeric `vm` id (in a server, usually
  one per request). VMs stop and resume independently: one stopped at a
  breakpoint doesn't freeze the others, and `continue` resumes only the VM it
  names.
- **Frame**: `0` is the innermost frame of a stopped VM.
- **Ref**: a positive number that names an expandable value (array, map,
  object, scope). Valid only while its VM stays stopped. `0`: not expandable.
- **Paths**: relative to the program's working directory (`cwd` in `hello`).
  Requests use the same form.

### Types

```
Frame = {"function": "save", "file": "lib/orders.ts", "line": 12}

Var = {
  "name": "customer",
  "value": "{id: 3, name: \"Ann\", …}",  // preview, already formatted
  "type": "map",
  "ref": 1042,                          // 0 if not expandable
  "count": 12                           // children, when ref > 0
}

Stop = {
  "vm": 3,
  "reason": "breakpoint" | "step" | "pause" | "exception" | "entry",
  "file": "lib/orders.ts",
  "line": 12,
  "exception": {"message": "...", "stack": "..."},  // reason exception only
  "frames": [Frame, ...],                           // frame 0 first
  "locals": [Var, ...],                             // locals of frame 0
  "globals": 1043                                   // ref: frame 0's module variables
}
```

A stop carries everything needed to show it, so it costs no round trip.
`locals` lists the variables in scope at the stop, in declaration order.

### Requests

| cmd | arguments | result |
|-----|-----------|--------|
| `hello` | `version` | `version`, `cwd`, `waiting` (held before running), `running` (VMs running), `stopped: [Stop]`, `page?` (the program's page, above) |
| `run` | `entry?` | Releases a program held before running. Idempotent. With `entry`, the program stops at its entry (reason `entry`): the start of the code being debugged, which it decides, not the first code it runs. |
| `setBreakpoints` | `file`, `breakpoints: [{line, condition?, hit?, log?}]` | `breakpoints: [{line, error?}]`: where each one went, and why it was ignored |
| `setExceptions` | `uncaught`, `all` | |
| `continue` | `vm` | |
| `next` | `vm` | Step over. |
| `stepIn` | `vm` | |
| `stepOut` | `vm` | |
| `pause` | `vm?` | Stops `vm`, or the next VM that runs code when absent. |
| `runTo` | `vm`, `file`, `line` | Resumes `vm` until it reaches the line or stops for another reason. |
| `jump` | `vm`, `line` | Makes `line` of frame 0's function the next statement. Returns the `Stop` as it is now. |
| `frame` | `vm`, `frame` | `locals`, `globals` of that frame. |
| `expand` | `ref`, `start?`, `count?` | `vars: [Var]`, a page of the children. |
| `eval` | `vm`, `frame`, `expr` | A `Var` named `expr`. `a = expr` and `a.b[0] = expr` assign. |
| `threads` | | `running`, `stopped: [vm]` |
| `inspect` | `on` | The program's own, when it has one: an app with widgets lets the person pick one on its screen, and answers with a `reveal` of the line that made it. An unknown command is an error. |

`setBreakpoints` replaces every breakpoint of the file. A breakpoint on a line
without code may move to the next line with code; the result says where.
Breakpoint options:

- `condition`: an expression; the VM stops only when it is truthy.
- `hit`: `"5"` stops on the 5th hit, `">= 5"` from the 5th on, `"% 5"` every 5th.
- `log`: instead of stopping, an `output` event with the message, where
  `{expr}` is replaced by the value of `expr`.

`eval` and conditions should have no side effects other than an assignment
asked for.

### Events

| event | fields |
|-------|--------|
| `stopped` | a `Stop` |
| `resumed` | `vm`, sent before the VM runs again |
| `output` | `text`, `file`, `line`, `error?`: true for what went wrong in the program (the error that ended it), in red |
| `reveal` | `file`, `line`: a place the program asks to show. Den opens it in the editor. |

Programs that implement it: sim (`sim -d`, see its `debugger.md`).
