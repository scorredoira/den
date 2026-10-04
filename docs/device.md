# Device

The Device panel shows the screen of a phone, a simulator or an emulator, beside
the code. The mouse is a finger and the keyboard is the phone's, while Den's
shortcuts stay Den's: a tap stops at a breakpoint and F10 steps from the panel.

Den doesn't know iOS or Android. A device program serves the phones with the
protocol below; in the scl repo, `simview` (`native/tools/simview`) serves the
iOS simulator and `emuview` (`native/tools/emuview`) the Android emulator. The panel works only on macOS and for local workspaces: the
frames are IOSurfaces of this Mac.

## The device file

`.den/device.json` in the workspace names the device programs, relative to it:

```json
{ "programs": ["native/tools/simview/simview"] }
```

Without the file there is no panel and no icon: nothing changes for a workspace
that doesn't use it. On a server, or off macOS, there is no panel either. The
file is read again whenever it changes.

A program gets itself ready: a script that builds what's missing before running
it keeps Den out of building. Den runs it in the workspace's folder.

The panel asks each program for its devices when it first shows (Refresh in
the device menu asks again) and picks the first one listed. Start serves it;
the device menu serves another. Each workspace has its own panel; a program
keeps running while its workspace is open.

Building and opening the app on the device, and debugging it, belong in the
debugger's launch file (`docs/debugger.md`), not here: the panel only shows
the device and drives it. Its Inspect button asks the program being debugged
(the debugger's `inspect`) to let the person pick a widget on the phone; the
line that made it opens in the editor (`reveal`). With nothing being
debugged, the panel says so.

## The protocol

JSON, one object per line.

`<program> list` prints the devices it can serve, the booted first:

```json
{"id":"624D7FFC-…","name":"iPhone 17 Pro (iOS 26.2)","booted":true}
```

`<program> serve <id>` serves one: it starts it if needed, writes its events
to stdout and reads Den's commands from stdin. It ends when its stdin closes:
Den closes it to stop, and closing Den closes it too. What it writes to stderr
is shown when it ends.

Events:

- `{"size":[w,h],"bezel":b,"radius":r}`: the pixels of the screen, before its
  first frame and when they change. `bezel` and `radius`, optional, are the
  phone's edge in the same pixels: its width and its outer corner radius. Den
  draws it black around the screen; without them, the screen has no edge.
- `{"frame":n}`: a new frame is in the IOSurface number `n`. The surfaces are
  global (`kIOSurfaceIsGlobal`) so Den opens them by number, in YCbCr 4:2:0
  full range (`420f`), the format GPUI paints. A program keeps a few and
  writes them in turn; a new size brings new ones.
- `{"error":"…"}`: a command it could not do. It keeps going; Den shows it.

Commands:

- `{"touch":"down"|"move"|"up","x":…,"y":…}`: a finger at a point of the
  screen as shown, in fractions from its top left. With `"x2"` and `"y2"`, a
  second finger: a pinch. The panel pinches while Option is held, the second
  finger mirrored through the screen's centre, as Simulator.app does.
- `{"text":"…"}`: typed characters.
- `{"key":"enter","shift":false,"alt":false,"ctrl":false,"cmd":false}`: a key,
  one of `enter escape backspace tab space home pageup delete end pagedown
  right left down up`, or a single character, with the modifiers held.
- `{"button":"home"}`.
- `{"rotate":"left"|"right"}`: the device turned a quarter. The program sends
  its screen as held from then on, with a new `size` first, and takes points
  of the screen as shown.

## Keys

With the focus on the screen (click it), a character goes as text and Ctrl
with a character, or a named key, goes as a key. Den's shortcuts (F5, F9,
F10…) and anything with Cmd stay Den's, except Cmd-V, which pastes this Mac's
clipboard as text.
