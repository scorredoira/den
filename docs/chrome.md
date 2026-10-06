# Chrome

`den chrome` debugs the browser side of a web app in Chrome with Den's
debugger. It is a program that speaks the debug protocol v1
([debugger.md](debugger.md)) to Den and the Chrome DevTools Protocol (CDP) to
Chrome. Den doesn't change: it debugs `den chrome` like any other program. The
code of the pages is JavaScript bundled from TypeScript (esbuild, or any
bundler that writes source maps); breakpoints, stops and steps are on the
TypeScript lines. It knows nothing of any app.

## The command

```sh
den chrome --port P [--url URL] [--root DIR] [--headless] [--profile DIR] [--hosts LIST]
```

- `--port`: where it listens for Den, on 127.0.0.1. `0` takes a free port;
  the line `debugger listening on 127.0.0.1:P` says which.
- `--url`: the page to open. It opens when Den connects and sends `run`, so
  the breakpoints are in place for its first script (`waiting` is true in
  `hello` until then). A debugged tab of the same site (scheme, host and
  port) comes to the front and loads the URL again, so the code that runs at
  load meets the breakpoints and every session uses the same tab; with none, a
  new tab opens.
- `--root`: the folder the protocol's paths are relative to (`cwd` in
  `hello`). The working directory when missing.
- `--headless`: Chrome without windows, for tests and agents.
- `--profile`: Chrome's profile folder. By default a folder of its own in
  Den's data folder (`den/chrome`), kept between sessions so logins stay.
  Chrome 136 and later refuse remote debugging on the default profile.
- `--hosts`: the hosts whose pages are debugged, separated by commas: `name`,
  or `*.name` for its subdomains. By default `localhost,127.0.0.1,*.localhost`.

Chrome is `DEN_CHROME` when set, otherwise Google Chrome where it is usually
installed (on macOS `/Applications/Google Chrome.app`), or `google-chrome`,
`chromium` on the `PATH`. If a Chrome with the profile is running already
(its `DevToolsActivePort` answers), it is used; otherwise one is started with
`--remote-debugging-port=0`. When `den chrome` ends, a Chrome with windows
stays open, like a booted simulator; a headless one it started is closed.
When Chrome closes, `den chrome` ends.

The launch file for an app served on port 9092:

```json
{ "command": "den chrome --port ${port} --url http://localhost:9092/" }
```

## Pages

Each tab is a VM of the protocol, with its id while the tab lives. New tabs
and navigations are picked up: a new tab waits until it is set up, so its
first script stops at its breakpoints too. A script is debugged when the
origin it runs in is one of `--hosts`; scripts of other sites run on and
never stop. `pause` without `vm` stops the next debugged tab that runs code.

## Source maps

When a script loads, its source map is read: a `data:` URL (base64 or plain)
or an `http:` URL, relative to the script's. Each source of the map is a file
under the root: schemes (`webpack://`, `file://`) and `.`, `..` parts are
dropped, and the longest end of the path that exists under the root is the
file. Sources that name no file under the root are ignored: their code is
code without a mapping.

Chrome holds every script with a source map before it runs, until its
breakpoints are placed. A breakpoint goes to the first generated position of
its line, or of the next line with code; the result of `setBreakpoints` says
which line. A breakpoint in a file no script has loaded yet waits for one. A
reload places them again.

Stops show the original file and line of each frame; a frame without a
mapping shows the script's URL and its line. A step that lands in code without
a mapping goes on until code with one, and a step that stays on the same
TypeScript line goes on too (a line can be several JavaScript statements), up
to 300 steps. `next` and `stepOut` that return to a caller go on to its next
line, as sim does.

## Values

`locals` are the variables of the frame, its blocks and its closures, outer
ones first, and `this` in a method. `globals` is the module's variables: the
scope of the bundle's wrapper (a function of no source), or the module's or
script's scope. Arrays, maps and sets carry `count`; a map's children are its
entries, named by their key. A getter is shown as `(…)`, not run.

`eval`, log messages and conditions are evaluated in the frame. `eval` and log
messages refuse an expression with side effects; an assignment (`a = 1`,
`a.b[0] = 1`) runs anyway. Conditions are Chrome's own: Chrome evaluates them
without the side-effect check. Hit counts and log messages are counted and
written by `den chrome`.

`output` events are the console of the debugged pages (`console.log` and the
rest, with the file and line of the call) and their uncaught exceptions,
unless the VM stopped for that exception.

## Its own commands

For tests and agents, beside the protocol's. `vm` is the first debugged tab
when missing.

| cmd | arguments | result |
|-----|-----------|--------|
| `navigate` | `url`, `vm?` | Loads the URL in the tab. |
| `reload` | `vm?` | Reloads the tab. |
| `evaluate` | `expr`, `vm?` | Runs the expression in the page, awaiting a promise: a `Var` named `expr`, with no `ref`. It answers when the code ends, after any stop it makes. |
| `pages` | | `pages: [{vm, url, title}]` |

`DEN_CHROME_TRACE=1` prints every CDP message on stderr.

## Limits

- `jump` is an error: Chrome can't set the next statement.
- `inspect` is an unknown command.
- `run` with `entry` doesn't stop at an entry: it opens `--url`.
- Source maps over `https:` are not read, nor index maps (with `sections`).
- Names are the bundle's: a variable the bundler renamed (`name2`) has that
  name in `locals` and `eval`.
- Workers and frames of other sites are not debugged.

The code is in `crates/chrome`; its tests debug a fixture page in a headless
Chrome (`cargo test -p chrome`).
