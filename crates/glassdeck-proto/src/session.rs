use std::fmt;

use crate::{Frame, Message, PutFile, StreamInfo};

/// Where a receiver is in a connection's sequence of streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Streaming(StreamInfo),
}

/// What the receiver must do in response to a message.
#[derive(Debug, PartialEq, Eq)]
pub enum Action<'a> {
    /// Reset the decoder for a new stream (first stream, or a source switch).
    Begin(StreamInfo),
    Decode(&'a Frame),
    End,
    /// Reply to a clock probe at once with `ClockReply`, stamped with the probe's arrival time.
    AnswerProbe { probe_sent_us: u64 },
    /// Make this the background shown when no stream is open; the session itself doesn't change.
    SelectBackground(&'a str),
    /// Hand a widget instance what its host half sent; widgets run whether or not a stream is open.
    ModuleData { instance: u16, payload: &'a [u8] },
    /// Write a piece of a file; files arrive whether or not a stream is open.
    PutFile(&'a PutFile),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionError {
    FrameWithoutStream,
    EndWithoutStream,
    WrongStream { current: u32, got: u32 },
    ProbeWhileStreaming,
    UnexpectedReply,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameWithoutStream => write!(f, "frame received before StreamStart"),
            Self::EndWithoutStream => write!(f, "StreamEnd received with no stream open"),
            Self::WrongStream { current, got } => write!(f, "message for stream {got} while stream {current} is open"),
            Self::ProbeWhileStreaming => write!(f, "clock probe received while a stream is open"),
            Self::UnexpectedReply => write!(f, "replies only travel from receiver to sender"),
        }
    }
}

impl std::error::Error for SessionError {}

/// Every move between session states goes through here; the test walks the full table.
pub fn step<'a>(state: SessionState, message: &'a Message) -> Result<(SessionState, Action<'a>), SessionError> {
    match (state, message) {
        (_, Message::StreamStart(info)) => Ok((SessionState::Streaming(*info), Action::Begin(*info))),
        (_, Message::ClockReply { .. } | Message::PutDone { .. } | Message::PutRefused { .. }) => Err(SessionError::UnexpectedReply),
        (_, Message::PutFile(put)) => Ok((state, Action::PutFile(put))),
        (_, Message::SelectBackground { name }) => Ok((state, Action::SelectBackground(name))),
        (_, Message::ModuleData { instance, payload }) => Ok((state, Action::ModuleData { instance: *instance, payload })),
        (SessionState::Idle, Message::ClockProbe { sent_us }) => Ok((state, Action::AnswerProbe { probe_sent_us: *sent_us })),
        (SessionState::Streaming(_), Message::ClockProbe { .. }) => Err(SessionError::ProbeWhileStreaming),
        (SessionState::Idle, Message::Frame(_)) => Err(SessionError::FrameWithoutStream),
        (SessionState::Idle, Message::StreamEnd { .. }) => Err(SessionError::EndWithoutStream),
        (SessionState::Streaming(info), Message::Frame(frame)) => {
            check_stream(info, frame.stream_id)?;
            Ok((state, Action::Decode(frame)))
        }
        (SessionState::Streaming(info), Message::StreamEnd { stream_id }) => {
            check_stream(info, *stream_id)?;
            Ok((SessionState::Idle, Action::End))
        }
    }
}

fn check_stream(info: StreamInfo, got: u32) -> Result<(), SessionError> {
    match info.stream_id == got {
        true => Ok(()),
        false => Err(SessionError::WrongStream { current: info.stream_id, got }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Codec, FileTarget, Mode};

    fn info(stream_id: u32) -> StreamInfo {
        StreamInfo { stream_id, codec: Codec::H264, mode: Mode::Live { clock_offset_us: 0 }, width: 2, height: 2 }
    }

    fn frame(stream_id: u32) -> Message {
        Message::Frame(Frame { stream_id, seq: 0, time_us: 0, keyframe: false, payload: vec![] })
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Expect {
        Moves(SessionState, &'static str),
        Refused(SessionError),
    }

    fn outcome(state: SessionState, message: &Message) -> Expect {
        match step(state, message) {
            Ok((next, Action::Begin(_))) => Expect::Moves(next, "begin"),
            Ok((next, Action::Decode(_))) => Expect::Moves(next, "decode"),
            Ok((next, Action::End)) => Expect::Moves(next, "end"),
            Ok((next, Action::AnswerProbe { .. })) => Expect::Moves(next, "answer probe"),
            Ok((next, Action::SelectBackground(_))) => Expect::Moves(next, "select background"),
            Ok((next, Action::ModuleData { .. })) => Expect::Moves(next, "module data"),
            Ok((next, Action::PutFile(_))) => Expect::Moves(next, "put file"),
            Err(e) => Expect::Refused(e),
        }
    }

    #[test]
    fn walks_the_whole_transition_table() {
        use SessionState::{Idle, Streaming};
        let a = info(1);
        let b = info(2);
        let probe = Message::ClockProbe { sent_us: 5 };
        let reply = Message::ClockReply { probe_sent_us: 5, received_us: 6 };
        let select = Message::SelectBackground { name: "a.deckbg".into() };
        let data = Message::ModuleData { instance: 0, payload: vec![0xc0] };
        let put = Message::PutFile(PutFile { target: FileTarget::Profile, offset: 0, total: 1, data: vec![b'{'] });
        let done = Message::PutDone { target: FileTarget::Profile };
        let refused = Message::PutRefused { target: FileTarget::Profile, reason: "full".into() };
        let table = [
            (Idle, put.clone(), Expect::Moves(Idle, "put file")),
            (Streaming(a), put, Expect::Moves(Streaming(a), "put file")),
            (Idle, done.clone(), Expect::Refused(SessionError::UnexpectedReply)),
            (Streaming(a), done, Expect::Refused(SessionError::UnexpectedReply)),
            (Idle, refused.clone(), Expect::Refused(SessionError::UnexpectedReply)),
            (Streaming(a), refused, Expect::Refused(SessionError::UnexpectedReply)),
            (Idle, data.clone(), Expect::Moves(Idle, "module data")),
            (Streaming(a), data, Expect::Moves(Streaming(a), "module data")),
            (Idle, select.clone(), Expect::Moves(Idle, "select background")),
            (Streaming(a), select, Expect::Moves(Streaming(a), "select background")),
            (Idle, probe.clone(), Expect::Moves(Idle, "answer probe")),
            (Idle, reply.clone(), Expect::Refused(SessionError::UnexpectedReply)),
            (Streaming(a), probe, Expect::Refused(SessionError::ProbeWhileStreaming)),
            (Streaming(a), reply, Expect::Refused(SessionError::UnexpectedReply)),
            (Idle, Message::StreamStart(b), Expect::Moves(Streaming(b), "begin")),
            (Idle, frame(1), Expect::Refused(SessionError::FrameWithoutStream)),
            (Idle, Message::StreamEnd { stream_id: 1 }, Expect::Refused(SessionError::EndWithoutStream)),
            (Streaming(a), Message::StreamStart(b), Expect::Moves(Streaming(b), "begin")),
            (Streaming(a), frame(1), Expect::Moves(Streaming(a), "decode")),
            (Streaming(a), frame(2), Expect::Refused(SessionError::WrongStream { current: 1, got: 2 })),
            (Streaming(a), Message::StreamEnd { stream_id: 1 }, Expect::Moves(Idle, "end")),
            (
                Streaming(a),
                Message::StreamEnd { stream_id: 2 },
                Expect::Refused(SessionError::WrongStream { current: 1, got: 2 }),
            ),
        ];
        for (from, message, expected) in table {
            assert_eq!(outcome(from, &message), expected, "from {from:?} on {message:?}");
        }
    }
}
