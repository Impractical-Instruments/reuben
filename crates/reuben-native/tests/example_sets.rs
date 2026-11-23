//! Every line of the example set files under `sets/` compiles, live line and commented-out
//! alternative alike, and reads in the mode of the input it targets.

use std::path::Path;

use reuben_pattern::{Pattern, Value};

/// Cycles queried to see every atom a `<>` or `|` can reach.
const CYCLES: i64 = 8;

/// What a note name means at an input: only `/notes/in` plays pitches, and the scale-degree input
/// takes whole numbers.
fn check_mode(address: &str, pattern: &Pattern) -> Result<(), String> {
    let mut notes = vec![reuben_pattern::Note::EMPTY; 512];
    for cycle in 0..CYCLES {
        let got = pattern.cycle(cycle, 0, &mut notes);
        for note in &notes[..got.written] {
            match (address, note.value) {
                ("/notes/in", _) => {}
                (_, Value::Note(_)) => {
                    return Err(format!("a note name on {address}, which reads numbers"))
                }
                ("/pattern/in", Value::Number(n)) if n.to_f64().fract() != 0.0 => {
                    return Err(format!("{n:?} is not a whole scale degree on {address}"))
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// `(line number, address, text)` for each live line and each `# /address  text` alternative.
fn sends(source: &str) -> Vec<(usize, String, String)> {
    source
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let body = line
                .strip_prefix("# /")
                .map_or(line, |rest| &line[line.len() - rest.len() - 1..]);
            if !body.starts_with('/') {
                return None;
            }
            let (address, text) = body.split_once(char::is_whitespace)?;
            Some((i + 1, address.to_string(), text.trim().to_string()))
        })
        .collect()
}

#[test]
fn every_example_set_line_compiles_for_its_input() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sets");
    let mut files = 0;
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("the sets directory") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("reuben") {
            continue;
        }
        files += 1;
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let source = std::fs::read_to_string(&path).expect("read the set");
        let found = sends(&source);
        for (line, address, text) in found {
            let pattern = Pattern::new(&text)
                .unwrap_or_else(|e| panic!("{name}:{line}: {address}: {e}: {text}"));
            check_mode(&address, &pattern).unwrap_or_else(|e| panic!("{name}:{line}: {e}: {text}"));
            checked += 1;
        }
    }
    assert!(files >= 2, "both sets are there");
    assert!(checked > 40, "the alternatives are checked too: {checked}");
}

#[test]
fn the_checker_reads_the_lines_it_should() {
    let found = sends("# header /x\n#   /a  skip\n/a  0 1\n# /b  c3 [eb3]\n");
    assert_eq!(
        found,
        vec![
            (3, "/a".to_string(), "0 1".to_string()),
            (4, "/b".to_string(), "c3 [eb3]".to_string())
        ]
    );
}

#[test]
fn the_mode_check_rejects_a_note_name_where_numbers_are_read() {
    let name = Pattern::new("c3 4").expect("compiles");
    assert!(check_mode("/notes/in", &name).is_ok());
    assert!(check_mode("/cutoff/in", &name).is_err());
    let fraction = Pattern::new("0.5").expect("compiles");
    assert!(check_mode("/pattern/in", &fraction).is_err());
}
