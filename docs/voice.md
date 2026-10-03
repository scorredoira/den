# Idea: voice dictation in Den

2026-10-03 · Status: idea, not planned yet

Push-to-talk dictation that writes wherever the focus is, built from the code of
[Handy](https://github.com/cjpais/Handy) (MIT, Rust/Tauri), local and offline.

## Why

Handy works well but often stops responding and has to be restarted. Den could
own the dictation and supervise it: notice when it hangs and restart it on its
own.

## Design

A third binary, `den-voice` (`crates/voice`), next to `den` and `den-agent`:

- Built in the same workspace and the same `cargo build` (`-p ui -p agent -p voice`),
  copied into `Den.app/Contents/MacOS/`. It is still one app to install and update.
- The heavy dependencies (ONNX Runtime, whisper.cpp, audio) are linked into
  `den-voice` only. `den` doesn't grow or compile slower. Handy links them
  statically into a single 41 MB binary, so this is feasible.
- It holds the model in memory (Parakeet V3, ~640 MB int8; Whisper optional),
  captures the microphone (`cpal`), trims silence (Silero VAD) and transcribes
  (`transcribe-rs`, Handy's crate).
- The protocol between `den` and `den-voice` lives in `proto`.

### Supervision

A crash in the C++ engines can't take down the UI, because they run in another
process. Den pings `den-voice` periodically. If the process dies, or doesn't
answer, or a transcription takes too long, Den kills and relaunches it. Reloading
the model takes a couple of seconds.

### Where the text goes

A global push-to-talk shortcut:

- **Den has the focus:** Den inserts the text directly into the focused terminal
  or editor. Nothing is simulated, so it also works in remote terminals (audio is
  transcribed on the Mac, only text travels).
- **Another app has the focus:** paste as Handy does: put the text on the
  clipboard, simulate Cmd-V, restore the clipboard. Needs the Accessibility
  permission only for this case.

This replaces Handy: one model in RAM and one shortcut. The trade-off is that
dictation only works while Den is open.

### First version

macOS only, Parakeet V3, reusing the models Handy already downloaded in
`~/Library/Application Support/com.pais.handy/models/`. A shortcut, a recording
indicator, and microphone permission (`NSMicrophoneUsageDescription`) in the
Info.plist. Model download in Settings, then Linux and Windows (`ort` in `./release`).

## Alternative: supervise Handy from outside

Den installs Handy, starts it with `--start-hidden` and restarts it on demand
(a command and a shortcut). It keeps working with Den closed, but Den can't detect
a hang: `handy --toggle-transcription` is fire-and-forget, so only a dead process
is visible.

## A lead on the hangs

`~/Library/Logs/com.pais.handy/handy.log` has ~50 of these:

```
SecureInput held for 3s — keyed shortcuts are blocked; activating fallback
SecureInput fallback: 'transcribe' ('fn+f18') cannot be expressed via Carbon
```

While some app holds macOS Secure Input (password fields, "Secure Keyboard
Entry" in a terminal, password managers), keyboard event taps are blocked and
Handy's shortcut stops working, which looks like a hang. Its fallback can't
register `fn+F18`. Find out who holds it with
`ioreg -l -w 0 | grep kCGSSessionSecureInputPID`. Worth checking before copying
Handy's shortcut code, and worth trying a shortcut without `fn` in Handy today.

## License

Handy is MIT and Den is GPL-3.0-or-later. MIT code can be incorporated if
Handy's copyright notice is kept.
