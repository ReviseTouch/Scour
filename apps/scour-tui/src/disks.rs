//! The disks: a panel that switches them, and the question about one that
//! came. While a question waits, the panel's title is the question and its
//! first three lines are the answers; the disks follow.

use scour_proto::{Disk, DiskState};

use crate::app::{App, Panel, Want};

/// The answers, in the order they are drawn, as the service takes them.
const ANSWERS: [(&str, &str); 3] = [
    ("index", "Index it"),
    ("later", "Not now"),
    ("leave", "Never"),
];

impl App {
    /// The disk waiting for an answer, if one is.
    pub fn asked_disk(&self) -> Option<&Disk> {
        self.disks.iter().find(|d| d.state == DiskState::Asking)
    }

    /// The question in this language, for the panel and the counter line.
    pub fn disk_question(&self) -> Option<String> {
        let d = self.asked_disk()?;
        Some(
            self.say(scour_ui::disks::question(d.volume.kind.as_str()))
                .replace("{name}", d.volume.name()),
        )
    }

    fn answers(&self) -> usize {
        if self.asked_disk().is_some() {
            ANSWERS.len()
        } else {
            0
        }
    }

    pub fn disk_lines(&self) -> usize {
        self.answers() + self.disks.len()
    }

    pub fn open_disks(&mut self) -> Want {
        self.show(Panel::Disks);
        Want::Disks
    }

    pub fn disks_came(&mut self, disks: Vec<Disk>) {
        self.disks = disks;
        self.panel_at = self.panel_at.min(self.disk_lines().saturating_sub(1));
        self.dirty = true;
    }

    /// How many wait for an answer, from a status: when that moves, the list.
    pub fn disks_moved(&mut self, asking: u32) -> Want {
        if self.disks_asking == Some(asking) {
            return Want::Nothing;
        }
        self.disks_asking = Some(asking);
        Want::Disks
    }

    /// Press the line the cursor is on: an answer, or a disk to switch.
    pub fn press_disk(&mut self) -> Want {
        let answers = self.answers();
        if self.panel_at < answers {
            let Some(d) = self.asked_disk() else {
                return Want::Nothing;
            };
            return Want::Disk {
                volume: d.volume.id.clone(),
                what: ANSWERS[self.panel_at].0.into(),
            };
        }
        match self.disks.get(self.panel_at - answers) {
            Some(d) if d.state != DiskState::Configured => Want::Disk {
                volume: d.volume.id.clone(),
                what: if d.state.indexed() { "leave" } else { "index" }.into(),
            },
            _ => Want::Nothing,
        }
    }

    /// The panel's title, and its lines: the text, whether it is dimmed, and
    /// whether a rule goes above it.
    pub fn disk_panel(&self) -> (String, Vec<(String, bool, bool)>) {
        let title = self
            .disk_question()
            .unwrap_or_else(|| self.say("DISKS").into_owned());
        let mut lines: Vec<(String, bool, bool)> = Vec::new();
        if self.asked_disk().is_some() {
            for (_, msgid) in ANSWERS {
                lines.push((self.say(msgid).into_owned(), false, false));
            }
        }
        let mark = self.mark().1;
        for (at, d) in self.disks.iter().enumerate() {
            let v = &d.volume;
            let mut text = format!(
                "{} {}  ·  {}",
                if d.state.indexed() { "☑" } else { "☐" },
                v.name(),
                self.say(scour_ui::disks::kind(v.kind.as_str()))
            );
            if v.size > 0 {
                text.push_str("  ·  ");
                text.push_str(&scour_ui::format::compact_bytes(v.size, mark));
            }
            text.push_str("  ·  ");
            text.push_str(&self.say(scour_ui::disks::state(d.state.as_str())));
            let rule = at == 0 && !lines.is_empty();
            lines.push((text, d.state == DiskState::Configured, rule));
        }
        (title, lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_proto::{Volume, VolumeKind};

    fn disk(id: &str, state: DiskState) -> Disk {
        Disk {
            volume: Volume {
                id: id.into(),
                roots: vec![format!("/{id}")],
                kind: VolumeKind::Removable,
                ..Volume::default()
            },
            state,
        }
    }

    #[test]
    fn the_answers_come_first_while_a_disk_waits() {
        let mut app = App {
            disks: vec![
                disk("a", DiskState::Configured),
                disk("b", DiskState::Asking),
            ],
            ..App::default()
        };
        assert_eq!(app.open_disks(), Want::Disks);
        assert_eq!(app.disk_lines(), 5);
        app.panel_at = 1;
        let later = Want::Disk {
            volume: "b".into(),
            what: "later".into(),
        };
        assert_eq!(app.press_disk(), later);
        app.panel_at = 3;
        assert_eq!(
            app.press_disk(),
            Want::Nothing,
            "config.toml decides that one"
        );
        app.panel_at = 4;
        let yes = Want::Disk {
            volume: "b".into(),
            what: "index".into(),
        };
        assert_eq!(app.press_disk(), yes);
    }
}
