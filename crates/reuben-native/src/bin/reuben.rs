//! `reuben` — play an instrument live, driven by OSC over UDP.
//!
//! Listens for OSC on UDP `0.0.0.0:9000` and renders an instrument to the default audio
//! device. With no argument it plays the built-in default rig; pass a path to load a
//! different instrument JSON: `reuben path/to/instrument.json`. Play a note by sending:
//!
//! ```text
//! /voicer/note  [69.0, 1.0]   # note-on  (MIDI 69 = A4, gate 1)
//! /voicer/note  [69.0, 0.0]   # note-off (gate 0)
//! ```
//!
//! Any OSC source works (TouchOSC, a Max/Pd patch, `oscsend`, a Python script).

use std::net::UdpSocket;
use std::sync::mpsc;
use std::thread;

use reuben_core::plan::Plan;
use reuben_core::{load, Registry};
use reuben_native::engine::Engine;
use reuben_native::rigs::DEFAULT_JSON;
use reuben_native::{audio, osc};

const BLOCK_SIZE: usize = 256;
const OSC_BIND: &str = "0.0.0.0:9000";

fn main() {
    let (tx, rx) = mpsc::channel();

    // Log incoming OSC only when asked: this runs on the receive path, and the stdout
    // lock would add latency/jitter if it fired on every note while playing. Off by
    // default; flip on to confirm wiring during bring-up.
    let log_osc = std::env::var_os("REUBEN_LOG_OSC").is_some();

    // OSC/UDP receiver thread: decode datagrams and forward Messages to the audio thread.
    let socket = UdpSocket::bind(OSC_BIND).expect("bind OSC socket");
    println!("OSC-in listening on {OSC_BIND}  (send /voicer/note [midi, gate])");
    if !log_osc {
        println!("  (set REUBEN_LOG_OSC=1 to log received OSC)");
    }
    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            match socket.recv_from(&mut buf) {
                Ok((n, _)) => match osc::decode(&buf[..n]) {
                    Ok(msgs) => {
                        for m in msgs {
                            if log_osc {
                                println!("recv {} {:?}", m.addr, m.args.as_slice());
                            }
                            let _ = tx.send(m);
                        }
                    }
                    Err(e) => eprintln!("OSC decode error: {e}"),
                },
                Err(e) => {
                    eprintln!("OSC recv error: {e}");
                    break;
                }
            }
        }
    });

    // Instrument source: a path argument, else the embedded default.
    let instrument_json = match std::env::args().nth(1) {
        Some(path) => {
            println!("instrument: {path}");
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
        }
        None => {
            println!("instrument: <default> (pass a path to load your own)");
            DEFAULT_JSON.to_string()
        }
    };

    let _stream = audio::start(rx, BLOCK_SIZE, |cfg| {
        println!(
            "audio out @ {} Hz, block {}",
            cfg.sample_rate, cfg.block_size
        );
        let graph = load(&instrument_json, &Registry::builtin()).expect("load instrument");
        let plan = Plan::instantiate(graph, cfg).expect("instantiate rig");
        Engine::new(plan)
    })
    .expect("start audio");

    println!("playing — Ctrl-C to quit.");
    // Keep the process (and thus the audio stream) alive.
    loop {
        thread::park();
    }
}
