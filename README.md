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

Then clone this repo. There is no install step beyond that: the commands below build and run.

Each tagged release also ships a prebuilt `reuben` binary for Linux and Windows on the
[Releases page](https://github.com/Impractical-Instruments/reuben/releases). Run it from a
terminal as `./reuben play` (`reuben.exe play` on Windows).

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
keyboard for it. Linux only for now, and it needs a desktop session (X11 or Wayland).

```sh
cargo run -p reuben-editor -- instruments/groovebox.json
```

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
