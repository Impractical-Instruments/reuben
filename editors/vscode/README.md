# Live coding from VS Code

Two tasks and two key bindings that drive a running `reuben play` from VS Code with no extension:

- **reuben live** runs `reuben live ${file}` in the background. Each save sends the set file's
  changed lines, and a line the engine refuses shows as an error at its span in the editor. The
  error clears on the next save that fixes it.
- **reuben send selection** sends the selected text to one address. VS Code asks for the address
  and pre-fills the one you last entered in this window.

## Set files

A set file holds one send per line: an address, whitespace, then the pattern text to the end of the
line. Blank lines and lines starting with `#` are skipped. These recipes use the `.reuben`
extension, and the key bindings only fire in files that have it. [`example.reuben`](example.reuben)
is a starting point.

`/pat/in` in `example.reuben` (and the address prompt's default) is a placeholder. The library
instrument that plays a `pattern` Operator is not merged yet. Once it is, point the line at that
instrument's pattern input. `reuben describe <instrument> --view pipes` lists its inputs.

## Setup

1. Put `reuben` on your `PATH`, or replace `"command": "reuben"` in `tasks.json` with the binary's
   full path.
2. Copy [`tasks.json`](tasks.json) into your set folder as `.vscode/tasks.json`. If you already
   have one, merge its `tasks` and `inputs` entries into yours.
3. Add the entries in [`keybindings.json`](keybindings.json) to your own key bindings
   (**Preferences: Open Keyboard Shortcuts (JSON)**).
4. Start `reuben play` with the instrument you want to play, in a terminal.
5. Open a `.reuben` file and press `ctrl+shift+enter` (or run **Tasks: Run Task → reuben live**).
   Select some text and press `ctrl+enter` to send just that.

Both tasks reach the engine at its default address. To reach another one, add
`"--engine-addr", "<host:port>"` after `"live"` or `"send"` in `args`, or set `REUBEN_ENGINE_ADDR`.

## How the error marks work

`reuben live` prints each problem as `<file>:<line>:<col>: error: <message>`, followed by the
refused text with its span marked. It wraps each save's sends in a `reuben live: sending <file>`
line and a `reuben live: done` line. The task's problem matcher reads the location lines. It treats
the sending/done pair as the begin and end of one round, so problems a later round no longer reports
are cleared. A refused line is sent again on every save until it is fixed, so it stays marked until
then.

A Swap that lands between a send's compile and its apply drops that send. `reuben live` names it
as a `warning:` at its line, and the next save sends it again.
