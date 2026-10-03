# Debugger

Den debugs any program that speaks the protocol below: Den doesn't know its
language or its VM. The program listens on a TCP port; Den reaches it through
the agent, so a program on a server is debugged like a local one.

## The launch file

`.den/debug.json` in the workspace:

```json
{ "command": "sim -d ${file}", "port": 4444 }
```

- `command`: a shell line run in a terminal of the workspace (its output stays
  there). `${file}` is the open file, relative to the workspace: there is one
  command, which debugs whatever is open, and the program decides what that
  means (a script, a test file, the server it is part of). Without `command`,
  F5 attaches to a program already running.
- `port`: where the program listens, on the loopback of the agent's machine.
  4444 when missing.

F5 runs the command. If something already answers on the port, it attaches to
it instead of starting the command again. A program held before running
(`waiting` in `hello`) is released with `entry`, so it stops at its entry, as
Visual Studio's debugger does: where the program says (sim: the first line of
`main`, or of the function `-de` names). Stop (Shift-F5) interrupts a program it
started and leaves one it attached to running.

A file from when there were several `configurations` starts the first one.

### Tests

With a `tests` section, every line that declares a test gets Run (▷) and
Debug (the bug) at its end. Run starts `run` in a terminal; Debug starts `debug` under the
debugger. `${file}` is the open file and `${test}` the name `match` captured
(its first group). `port` (4444 when missing) is where `debug` listens: one
apart from the program's lets a test be debugged while it runs.

```json
"tests": {
    "match": "^export function (test\\w*)\\(",
    "run": "sim test ${file} ${test} -x",
    "debug": "sim -d -de ${test} -dp 127.0.0.1:4445 test ${file} ${test} -x -c 1",
    "port": 4445
}
```

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
| Cmd-Shift-Y | Show or hide the panel |

All of them can be changed in Settings.

It is a tab after the terminals' (closing the tab hides it; F5 or Cmd-Shift-Y
bring it back). Like every panel, it goes wherever its icon in the activity
bar is dragged: see Layout in the README.

When a VM stops, its line is marked, the values of the variables are written
at the end of the lines of its function, and hovering a name shows its value
in a card that opens like the variables view.
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
| `hello` | `version` | `version`, `cwd`, `waiting` (held before running), `running` (VMs running), `stopped: [Stop]` |
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
| `output` | `text`, `file`, `line` |

Programs that implement it: sim (`sim -d`, see its `debugger.md`).
