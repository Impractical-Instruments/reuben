# Live coding from VS Code

Three tasks and three key bindings that drive a running `reuben play` from VS Code with no
extension:

- **reuben send line** (`ctrl+enter` in a `.rpat` file) sends the pattern-file line the cursor is on:
  the line's address and text, with no prompt. It saves the file first, because `reuben send
  --file ${file} --line ${lineNumber}` reads the line from disk. That save also triggers `reuben
  live` if it is running, which sends any other changed lines too; the cursor's line then goes out
  once more, harmlessly. Success is silent. A line the engine refuses, an engine that is not
  running, and a comment or blank line (not a send) each open the Problems view and the terminal.
- **reuben live** (`ctrl+shift+enter`) runs `reuben live ${file}` in the background. Each save
  sends the pattern file's changed lines, and a line the engine refuses shows as an error at its span
  in the editor. The error clears on the next save that fixes it.
- **reuben send selection** (`ctrl+enter` with a selection, in files that are not `.rpat`) sends
  the selected text to an address VS Code asks for, such as `/notes/in`. A refusal opens the
  Problems view.

The `ctrl+enter` binding uses VS Code's
[`runCommands`](https://code.visualstudio.com/docs/configure/keybindings) command to run the save
and the task in order.

## Pattern files

A pattern file holds one send per line: an address, whitespace, then the pattern text to the end of the
line. Blank lines and lines starting with `#` are skipped. These recipes use the `.rpat`
extension, and the key bindings only fire in files that have it.

The repo's [`patterns/`](../../patterns) directory holds two starting points, each naming the instrument to
play and listing alternatives to swap in: [`pattern-melody.rpat`](../../patterns/pattern-melody.rpat)
(scale degrees, `/pattern/in`) and [`pattern-lab.rpat`](../../patterns/pattern-lab.rpat) (note
names, Hz and cutoff patterns). For another instrument, `reuben describe <instrument> --view
pipes` lists its inputs.

## Setup

### In the reuben repo

Open the repo root as the VS Code workspace. Its [`.vscode/tasks.json`](../../.vscode/tasks.json)
already holds the three tasks, and they run `reuben` through `cargo run`: the same binary
`cargo reuben-play` builds, so nothing needs installing. Each send waits on cargo's up-to-date
check, about a quarter of a second once the binary is built.

1. Add the entries in [`keybindings.json`](keybindings.json) to your own key bindings
   (**Preferences: Open Keyboard Shortcuts (JSON)**).
2. In a terminal: `cargo reuben-play instruments/pattern-lab.json`.
3. Open [`patterns/pattern-lab.rpat`](../../patterns/pattern-lab.rpat), put the cursor on a line, and
   press `ctrl+enter`.

Copy these tasks into your user tasks, or into another folder, too, and VS Code finds two tasks
with each name. `ctrl+enter` then asks which one to run.

### Anywhere else

1. Copy [`tasks.json`](tasks.json) into your pattern folder as `.vscode/tasks.json`. If you already
   have one, merge its `tasks` and `inputs` entries into yours.
2. Name the `reuben` binary in your settings (**Preferences: Open User Settings (JSON)**):
   `"reuben.command": "reuben"` if `reuben` is on your `PATH` (`cargo install --path
   crates/reuben-native --bin reuben` from the repo puts it there), else its full path. VS Code
   flags the setting as unknown, because no extension declares it, and reads it anyway. If it is
   unset, a task stops with "Variable ${config:reuben.command} can not be resolved".
3. Add the entries in [`keybindings.json`](keybindings.json) to your own key bindings.
4. Start an instrument in a terminal:
   `reuben play --instrument-root <repo>/instruments <repo>/instruments/pattern-melody.json`.
5. Open a `.rpat` file and put the cursor on a line: `ctrl+enter` sends it. Press
   `ctrl+shift+enter` (or run **Tasks: Run Task → reuben live**) to send the whole file now and on
   every save. Select text in another kind of file and press `ctrl+enter` to send it to an
   address.

A send that fails brings up the task's terminal, whatever the cause: a refused line, an engine
that is not running, or a binary that will not start.

The tasks reach the engine at its default address. To reach another one, add
`"--engine-addr", "<host:port>"` after `"live"` or `"send"` in the `args` of each task, or set
`REUBEN_ENGINE_ADDR`.

## How the error marks work

`reuben live` prints each problem as `<file>:<line>:<col>: error: <message>`, followed by the
refused text with its span marked. Only errors and warnings carry a location; a queued line prints
`reuben live: queued <address> (line <n>)`. It wraps each save's sends in a
`reuben live: sending <file>` line and a `reuben live: done` line. The task's problem matcher
reads the location lines. It treats
the sending/done pair as the begin and end of one round, so problems a later round no longer reports
are cleared. A refused line is sent again on every save until it is fixed, so it stays marked until
then.

A Swap that lands between a send's compile and its apply drops that send. `reuben live` names it
as a `warning:` at its line, and the next save sends it again.
