//! Cross-agent message board (subagent ↔ main coordination), with TTL.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BoardNote {
    pub id: u64,
    pub author: String,
    pub topic: String,
    pub body: String,
    pub ttl_turns: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageBoard {
    pub notes: Vec<BoardNote>,
    next_id: u64,
}

impl MessageBoard {
    pub fn post(&mut self, author: &str, topic: &str, body: &str, ttl_turns: Option<u64>) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.notes.push(BoardNote {
            id,
            author: author.into(),
            topic: topic.into(),
            body: body.into(),
            ttl_turns,
        });
        id
    }

    pub fn read_topic(&self, topic: &str) -> Vec<&BoardNote> {
        self.notes.iter().filter(|n| n.topic == topic).collect()
    }

    /// Age the board one agent turn; expired notes fall off.
    pub fn tick(&mut self) {
        let mut aged: Vec<BoardNote> = Vec::new();
        for n in self.notes.drain(..) {
            match n.ttl_turns {
                Some(t) if t <= 1 => {} // expired
                Some(t) => {
                    let mut n2 = n.clone();
                    n2.ttl_turns = Some(t - 1);
                    aged.push(n2);
                }
                None => aged.push(n),
            }
        }
        self.notes = aged;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_and_read_topics() {
        let mut b = MessageBoard::default();
        b.post("main", "errors", "compile failed at line 3", None);
        b.post("sub", "tests", "3 tests pass", None);
        assert_eq!(b.read_topic("errors").len(), 1);
        assert_eq!(b.read_topic("tests").len(), 1);
        assert_eq!(b.read_topic("none").len(), 0);
    }

    #[test]
    fn ttl_expires_notes() {
        let mut b = MessageBoard::default();
        b.post("sub", "progress", "halfway", Some(1));
        b.post("main", "progress", "note from main", None);
        b.tick();
        // ttl=1 note expired on first tick; the None-ttl note persists
        assert_eq!(b.read_topic("progress").len(), 1);
        b.tick();
        assert_eq!(b.read_topic("progress").len(), 1);
    }

    #[test]
    fn ids_monotonic() {
        let mut b = MessageBoard::default();
        let a = b.post("a", "t", "1", None);
        let c = b.post("a", "t", "2", None);
        assert!(c > a);
    }
}
