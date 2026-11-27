//! Getting a Flux keyboard ready for Glassdeck from the host: talk to its key scanner and its
//! stock bridge over USB, back both chips up, and run the whole first-day routine as one resumable
//! setup. The CLI and the app share it; neither prints from here, both are told through [`Note`]s
//! and setup [`Event`](setup::Event)s.

pub mod bridge;
mod flash_image;
mod picoboot;
pub mod scanner;
pub mod setup;
mod usb_command;

/// What a long-running call has to say while it works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// Something that happened: "saved flash.bin (16777216 bytes)".
    Said(String),
    /// How far a transfer has got; each one replaces the last until the next `Said`.
    Progress(String),
}

/// Where a call sends its [`Note`]s.
pub type Notes<'a> = &'a mut dyn FnMut(Note);

fn said(notes: Notes, text: impl Into<String>) {
    notes(Note::Said(text.into()));
}
