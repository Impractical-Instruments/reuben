# reuben

A configurable musical instrument built from composable **Operators**: small DSP units that
each do one simple thing and are patched together into playable Instruments. Rube Goldberg
machines, for music — hence "reuben." OSC is the lingua franca, in and out.

## Install

- **Rust** — install via [rustup](https://rustup.rs). The toolchain version is pinned by
  `rust-toolchain.toml` and picked up automatically.
- **Linux:** ALSA development headers.
  ```sh
  sudo apt-get install libasound2-dev      # Debian/Ubuntu
  ```
- **Windows:** nothing extra.
- **macOS:** the Xcode Command Line Tools (`xcode-select --install`).

Then clone this repo. There is no install step beyond that: the commands below build and run.

Or skip the build: see [Download a release](#download-a-release).

## Download a release

Each tagged release on the [Releases page](https://github.com/Impractical-Instruments/reuben/releases)
carries prebuilt archives, each with a `.sha256` file beside it:

| Archive | Platform |
| --- | --- |
| `reuben-<version>-x86_64-unknown-linux-gnu.tar.gz` | the CLI, Linux x86-64 |
| `reuben-<version>-x86_64-pc-windows-msvc.zip` | the CLI, Windows x86-64 |
| `reuben-editor-<version>-x86_64-unknown-linux-gnu.tar.gz` | the editor, Linux x86-64 |
| `reuben-editor-<version>-x86_64-pc-windows-msvc.zip` | the editor, Windows x86-64 |
| `reuben-editor-<version>-aarch64-apple-darwin.tar.gz` | the editor, macOS on Apple Silicon |

Check a download with `sha256sum -c <archive>.sha256` (`shasum -a 256 -c` on macOS). Run the
CLI from a terminal as `./reuben play` (`reuben.exe play` on Windows).

Every archive carries `THIRD-PARTY-NOTICES.txt` beside `LICENSE`: the licence of each Rust crate
built into that binary, for that platform.

The editor archive holds the binary, the instrument library (`instruments/`), the control
surfaces for it (`surfaces/`), and the licences of the fonts built into it (`font-licences/`).
Extract it anywhere and start `reuben-editor` from the extracted folder: its start screen offers
the library beside it.

The x86-64 builds need a CPU with AVX2 and FMA (the x86-64-v3 level): an Intel Core from
Haswell (2013) on, or an AMD from Excavator (2015) on. Some later Intel Pentium, Celeron and Atom
parts lack AVX2 and cannot run them.

**Linux.** Ubuntu 24.04 LTS or newer, or another distribution with glibc 2.39 or newer, and an
X11 or Wayland desktop session. A default Ubuntu Desktop install has everything the editor
loads. A server or minimal install has to add the desktop libraries, the OpenGL driver and
ALSA:

```sh
sudo apt-get install libasound2t64 libegl1 libgl1 libegl-mesa0 libgl1-mesa-dri libglx-mesa0 \
  libwayland-client0 libwayland-egl1 libx11-6 libx11-xcb1 libxcb1 libxcursor1 libxi6 \
  libxrender1 libxkbcommon0 libxkbcommon-x11-0 libdbus-1-3 xdg-desktop-portal \
  xdg-desktop-portal-gtk
```

`libdbus-1-3` and the two portal packages serve the editor's file dialogs, over the desktop
session's D-Bus.

The CLI needs only `libasound2t64`.

**Windows.** Windows 10 or 11, 64-bit, with a graphics driver that provides OpenGL 2.1 or newer
(a GPU vendor's driver does; a virtual machine's basic display adapter may not). The editor is
not signed, so SmartScreen may stop the first launch with "Windows protected your PC": choose
**More info**, then **Run anyway**. The editor opens no console window, so its log goes nowhere
by default. To keep it, start it from Command Prompt with its output redirected:

```bat
reuben-editor.exe > reuben-editor.log 2>&1
```

**macOS.** macOS 11 (Big Sur) or newer, on Apple Silicon. The editor is not signed or
notarized, so macOS blocks the first launch. Either clear the download's quarantine flag from
Terminal, in the extracted folder:

```sh
xattr -dr com.apple.quarantine .
./reuben-editor
```

or, in Finder, Control-click `reuben-editor` and choose **Open**, then confirm. On macOS 15
(Sequoia) and newer that menu no longer offers to open it: try once, then go to **System
Settings > Privacy & Security** and click **Open Anyway**.

The editor is a bare binary, not an app bundle, so macOS asks for microphone access on behalf
of the terminal app that starts it (Terminal, iTerm, …), the first time an instrument with a
microphone input plays. If that was declined, input is silent: allow the terminal app under
**System Settings > Privacy & Security > Microphone** and restart the editor.

## Launch

Play the default synth:

```sh
cargo reuben-play
```

Play an instrument from the library. This one makes sound on its own:

```sh
cargo reuben-play instruments/groovebox.json
```

Either one opens your default audio device and listens for OSC on UDP port 9000. Play a note
on the default synth by sending `/notes/in [midi, gate]` from any OSC sender: `[69.0, 1.0]` is
note-on A4, `[69.0, 0.0]` is note-off.

`cargo reuben-play` is an alias for `cargo run -p reuben-native --bin reuben -- play`. The
`reuben` binary has other subcommands (`describe`, `validate`, `new-instrument`,
`scaffold-operator`); add `--help` to any of them.

### The editor

A desktop app that opens an instrument, plays it, and gives you on-screen controls and a
keyboard for it. It runs on Linux (in an X11 or Wayland session), Windows and macOS; a release
carries a prebuilt one for each (see [Download a release](#download-a-release)). To run it from
source:

```sh
cargo run -p reuben-editor -- instruments/groovebox.json
```

Without a path it opens on a start screen: play an instrument from the library (the
`--instrument-root` you name, else `instruments/` in the working directory or beside the
`reuben-editor` binary), create a new one, or reopen a recent file.

If UDP port 9000 is already taken, by `reuben play` or another editor, the editor starts without
OSC-in and says so; instruments still open, play and edit.

The engine follows the files: a change to the instrument, or to any voice or subpatch file it
references — from `reuben-mcp`, a text editor, anything — is swapped in as it is saved, and the
editor shows it. A change that does not load is shown, and the last instrument that loaded keeps
playing.

#### Attaching to a running engine

The editor can also play an engine it did not start, such as a `reuben play` started by a
launcher script for a show. It reaches that engine on its structure channel, which `reuben play`
opens on `127.0.0.1:9124`:

```sh
cargo reuben-play instruments/groovebox.json  # elsewhere, already running
cargo run -p reuben-editor -- --attach 127.0.0.1:9124
```

The editor opens the instrument the engine plays, so its controls play the engine live, and an
edit is swapped in as usual. The audio device and the engine flags (`--io-map`, `--osc-out`) stay
the ones `reuben play` was started with.

The editor checks the engine once a second. If another client has it play a different file, or it
is restarted on one, the editor opens that file rather than swapping its own over it. The engine
names every file it opens by one absolute path, however the client that asked spelled it, so a
file swapped in by a relative path opens too. If the engine stops answering, the editor says so in
its top bar and keeps trying. Closing the editor, or losing the link, never stops the engine.

Control values from the editor and from any other OSC sender reach the same inputs, and the
last one written wins. The editor does not hold a swap from another client to ask first, as it
does for an engine it started: whoever writes last wins.

The structure channel listens on loopback unless told otherwise. To attach from another machine,
start the engine on an address that machine can reach, with a token. The engine refuses to
listen beyond loopback without one, and serves a client from another machine only when the
client sends it:

```sh
# on the show machine
export REUBEN_STRUCTURE_TOKEN=$(openssl rand -hex 16)
cargo reuben-play instruments/groovebox.json --structure-addr 100.64.0.5:9124

# on the editing machine, with the same token
REUBEN_STRUCTURE_TOKEN=… cargo run -p reuben-editor -- --attach show-machine:9124
```

`--structure-addr` takes a literal IP: the machine's Tailscale or LAN address, or `0.0.0.0` for
every interface. `--token` works too, but anyone on the machine can read a flag in the process
list. `reuben-mcp` and the `reuben` CLI's `describe`, `validate` and `new-instrument` take the
same `REUBEN_STRUCTURE_TOKEN` (the CLI also takes `--token`), and `REUBEN_STRUCTURE_ADDR` or
`--structure-addr` to name the engine. A client on the engine's own machine needs no token.

The token travels in clear: the channel has no TLS. Tailscale encrypts the link. A plain LAN
does not, so use it only on a network you trust. An SSH tunnel still works too, and needs no
token, since the engine sees a loopback client:

```sh
ssh -N -L 9124:127.0.0.1:9124 show-machine
cargo run -p reuben-editor -- --attach 127.0.0.1:9124
```

Every control move waits on a round trip to the other machine, so expect some lag.

The editor reads and edits the instrument's files, controls included, only through the engine,
so a remote engine's files need not be on the editor's machine. The CLI sends a name to a remote
engine as you typed it, for the engine to resolve against what it offers.

The editor is in progress: editing the instrument's graph on a canvas is not there yet.

## More

- **[instruments/index.md](instruments/index.md)** — the instrument library, one line each.
- **[docs/agents/authoring.md](docs/agents/authoring.md)** — the instrument format and how to
  build or change an instrument.
- **[docs/agents/operator-dev.md](docs/agents/operator-dev.md)** — writing a new Operator in Rust.
- **[docs/device-profile.md](docs/device-profile.md)** — choosing an audio device and mapping
  its channels with `play --io-map`.
- **[.claude/skills/](.claude/skills/)** — agent skills for [Claude Code](https://claude.com/claude-code)
  that build instruments (`patcher`), make control surfaces for a phone or tablet
  (`control-surface`), and scaffold Operators (`create-operator`).
- **[CLAUDE.md](CLAUDE.md)** — the crate map and the commands used when working on the code.
- **[CONTRIBUTING.md](CONTRIBUTING.md)** — setup, toolchain, and the branch model.

## License

BSD 3-Clause. See [LICENSE](LICENSE).
