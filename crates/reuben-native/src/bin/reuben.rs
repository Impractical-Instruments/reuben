//! `reuben` — play the default rig live, driven by OSC over UDP.
//!
//! Listens for OSC on UDP `0.0.0.0:9000` and renders the default rig to the default audio
//! device. Play a note by sending, e.g.:
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
use reuben_native::engine::Engine;
use reuben_native::rigs::default_rig;
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

    let _stream = audio::start(rx, BLOCK_SIZE, |cfg| {
        println!(
            "audio out @ {} Hz, block {}",
            cfg.sample_rate, cfg.block_size
        );
        let plan = Plan::instantiate(default_rig(), cfg).expect("instantiate rig");
        Engine::new(plan)
    })
    .expect("start audio");

    println!("playing — Ctrl-C to quit.");
    // Keep the process (and thus the audio stream) alive.
    loop {
        thread::park();
    }
}
