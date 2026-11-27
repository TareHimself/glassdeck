use std::io::Write as _;

use glassdeck_setup::Note;
use glassdeck_setup::setup::{Event, Step};

/// Prints notes and setup events; a progress line is rewritten in place until the next line.
#[derive(Default)]
pub struct Printer {
    progress_open: bool,
}

impl Printer {
    pub fn note(&mut self, note: Note) {
        match note {
            Note::Said(text) => self.line(&text),
            Note::Progress(text) => {
                print!("\r{text}");
                let _ = std::io::stdout().flush();
                self.progress_open = true;
            }
        }
    }

    pub fn event(&mut self, event: Event) {
        match event {
            Event::Started(step) => self.line(&format!("== {}", name(step))),
            Event::Progress(_, note) => self.note(note),
            Event::NeedsYou(step, todo) => {
                self.line(&format!("{} needs you:", name(step)));
                for (i, item) in todo.iter().enumerate() {
                    self.line(&format!("  {}. {item}", i + 1));
                }
            }
            Event::Finished(step) => self.line(&format!("   {}: done", name(step))),
            Event::Failed(step, _) => self.line(&format!("   {}: failed", name(step))),
        }
    }

    fn line(&mut self, text: &str) {
        if std::mem::take(&mut self.progress_open) {
            println!();
        }
        println!("{text}");
    }
}

fn name(step: Step) -> &'static str {
    match step {
        Step::ScannerFound => "find the key scanner",
        Step::ScannerDriver => "check the boot ROM driver",
        Step::ScannerBackup => "back up the key scanner",
        Step::BridgeFound => "find the stock bridge",
        Step::Probe => "probe the Linux side",
        Step::LinuxBackup => "back up the Linux side",
    }
}
