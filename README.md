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

The editor archive holds the binary, the instrument library (`instruments/`), and the licences
of the two fonts built into it. Extract it anywhere and start `reuben-editor` from the extracted
folder: its start screen offers the library beside it.

The x86-64 builds need a CPU with AVX2 (Intel Haswell or AMD Excavator, 2013, or newer).

**Linux.** Ubuntu 24.04 LTS or newer, or another distribution with glibc 2.39 or newer, and an
X11 or Wayland desktop session. A default Ubuntu Desktop install has everything the editor
loads. A server or minimal install has to add the desktop libraries, the OpenGL driver and
ALSA:

```sh
sudo apt-get install libasound2t64 libegl1 libgl1 libegl-mesa0 libgl1-mesa-dri libglx-mesa0 \
  libwayland-client0 libwayland-egl1 libx11-6 libx11-xcb1 libxcb1 libxcursor1 libxi6 \
  libxrender1 libxkbcommon0 libxkbcommon-x11-0 libdbus-1-3 xdg-desktop-portal
```

The CLI needs only `libasound2t64`.

**Windows.** Windows 10 or 11, 64-bit, with a graphics driver that provides OpenGL 2.1 or newer
(a GPU vendor's driver does; a virtual machine's basic display adapter may not). The editor
opens no console window, so its log goes nowhere by default. To keep it, start it from Command
Prompt with its output redirected:

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

The editor follows the files: a change to the instrument, or to any voice or subpatch file it
references — from `reuben-mcp`, a text editor, anything — is swapped in as it is saved. A change
that does not load is shown, and the last instrument that loaded keeps playing.

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
