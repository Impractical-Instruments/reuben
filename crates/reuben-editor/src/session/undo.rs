//! The editor's undo and redo stacks: each entry is one write the editor made, named by the file it
//! went to and the content hashes on either side of it. No document text is held here; the texts
//! behind the hashes stay in the store `reuben-api` keeps for each file.

use reuben_api::authoring::RestoreInstrument;

/// One write the editor made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The document written, as the session's tree spells it.
    pub source: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Undo,
    Redo,
}

impl Step {
    /// The restore that carries this step's file across it in `direction`: guarded by the hash
    /// the file holds on this side, so a write made since by anyone else is not overwritten.
    pub fn restore(&self, direction: Direction) -> RestoreInstrument {
        let (to, expect) = match direction {
            Direction::Undo => (&self.before, &self.after),
            Direction::Redo => (&self.after, &self.before),
        };
        RestoreInstrument {
            source: self.source.clone(),
            to: to.clone(),
            expect: expect.clone(),
        }
    }

    /// The hash the file must hold for [`restore`](Self::restore) in `direction` to land.
    pub fn expect(&self, direction: Direction) -> &str {
        match direction {
            Direction::Undo => &self.after,
            Direction::Redo => &self.before,
        }
    }
}

/// The undo and redo stacks, newest last, and what the last undo or redo had to say.
#[derive(Debug, Default)]
pub struct Undo {
    undo: Vec<Step>,
    redo: Vec<Step>,
    note: Option<String>,
}

impl Undo {
    /// Take in a write the editor made. A new write is a new branch: what was undone before it
    /// can no longer be redone. A write that left the file as it was is no step.
    pub fn record(&mut self, step: Step) {
        if step.before == step.after {
            return;
        }
        self.undo.push(step);
        self.redo.clear();
        self.note = None;
    }

    /// The step an undo or redo would take next.
    pub fn next(&self, direction: Direction) -> Option<&Step> {
        self.stack(direction).last()
    }

    /// The next step in `direction` landed: it moves to the other stack.
    pub fn crossed(&mut self, direction: Direction) {
        if let Some(step) = self.stack_mut(direction).pop() {
            let other = match direction {
                Direction::Undo => Direction::Redo,
                Direction::Redo => Direction::Undo,
            };
            self.stack_mut(other).push(step);
        }
    }

    /// Drop the next step in `direction` and every step behind it, which can only be reached
    /// through it. Returns how many were dropped.
    pub fn forget(&mut self, direction: Direction) -> usize {
        let stack = self.stack_mut(direction);
        let dropped = stack.len();
        stack.clear();
        dropped
    }

    /// What the last undo or redo said, until the next one or a new write.
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    pub fn set_note(&mut self, note: Option<String>) {
        self.note = note;
    }

    fn stack(&self, direction: Direction) -> &Vec<Step> {
        match direction {
            Direction::Undo => &self.undo,
            Direction::Redo => &self.redo,
        }
    }

    fn stack_mut(&mut self, direction: Direction) -> &mut Vec<Step> {
        match direction {
            Direction::Undo => &mut self.undo,
            Direction::Redo => &mut self.redo,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(source: &str, before: &str, after: &str) -> Step {
        Step {
            source: source.to_string(),
            before: before.to_string(),
            after: after.to_string(),
        }
    }

    fn entries(undo: &Undo, direction: Direction) -> Vec<(&str, &str)> {
        undo.stack(direction)
            .iter()
            .map(|s| (s.source.as_str(), s.after.as_str()))
            .collect()
    }

    #[test]
    fn undo_restores_to_before_guarded_by_after_and_redo_the_reverse() {
        let s = step("a.json", "h0", "h1");
        let undo = s.restore(Direction::Undo);
        assert_eq!((undo.to.as_str(), undo.expect.as_str()), ("h0", "h1"));
        assert_eq!(s.expect(Direction::Undo), "h1");
        let redo = s.restore(Direction::Redo);
        assert_eq!((redo.to.as_str(), redo.expect.as_str()), ("h1", "h0"));
        assert_eq!(s.expect(Direction::Redo), "h0");
        assert_eq!(redo.source, "a.json");
    }

    #[test]
    fn steps_cross_between_the_stacks_newest_first() {
        let mut undo = Undo::default();
        undo.record(step("a.json", "a0", "a1"));
        undo.record(step("b.json", "b0", "b1"));
        assert_eq!(
            undo.next(Direction::Undo),
            Some(&step("b.json", "b0", "b1"))
        );
        assert_eq!(undo.next(Direction::Redo), None);

        undo.crossed(Direction::Undo);
        assert_eq!(
            undo.next(Direction::Undo),
            Some(&step("a.json", "a0", "a1"))
        );
        assert_eq!(
            undo.next(Direction::Redo),
            Some(&step("b.json", "b0", "b1"))
        );

        undo.crossed(Direction::Redo);
        assert_eq!(
            entries(&undo, Direction::Undo),
            [("a.json", "a1"), ("b.json", "b1")]
        );
        assert!(entries(&undo, Direction::Redo).is_empty());
    }

    #[test]
    fn a_new_write_clears_the_redo_stack_and_the_note() {
        let mut undo = Undo::default();
        undo.record(step("a.json", "a0", "a1"));
        undo.crossed(Direction::Undo);
        undo.set_note(Some("said".to_string()));
        undo.record(step("a.json", "a0", "a2"));
        assert_eq!(undo.next(Direction::Redo), None);
        assert_eq!(undo.note(), None);
        assert_eq!(entries(&undo, Direction::Undo), [("a.json", "a2")]);
    }

    #[test]
    fn a_write_that_changed_nothing_is_no_step_and_leaves_redo_alone() {
        let mut undo = Undo::default();
        undo.record(step("a.json", "a0", "a1"));
        undo.crossed(Direction::Undo);
        undo.record(step("a.json", "a0", "a0"));
        assert_eq!(undo.next(Direction::Undo), None);
        assert!(undo.next(Direction::Redo).is_some());
    }

    #[test]
    fn forgetting_a_step_drops_it_and_everything_behind_it_on_that_stack_only() {
        let mut undo = Undo::default();
        undo.record(step("a.json", "a0", "a1"));
        undo.record(step("b.json", "b0", "b1"));
        undo.record(step("a.json", "a1", "a2"));
        undo.crossed(Direction::Undo);
        assert_eq!(undo.forget(Direction::Undo), 2);
        assert_eq!(undo.next(Direction::Undo), None);
        assert!(undo.next(Direction::Redo).is_some());
    }
}
