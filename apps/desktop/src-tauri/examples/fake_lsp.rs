//! A stand-in language server for the LSP bridge's tests: it answers every message with
//! `{"echo": <message>}`, and exits with code 3 on `{"method":"exit"}`.

use std::io::{self, BufRead, Read, Write};

fn main() {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout().lock();
    loop {
        let mut length = None;
        loop {
            let mut line = String::new();
            if input.read_line(&mut line).expect("reading a header") == 0 {
                return;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                length = Some(value.trim().parse::<usize>().expect("a numeric length"));
            }
        }
        let mut body = vec![0; length.expect("a Content-Length header")];
        input.read_exact(&mut body).expect("reading a body");
        let message = String::from_utf8(body).expect("UTF-8");
        if message.contains(r#""method":"exit""#) {
            std::process::exit(3);
        }
        let answer = format!(r#"{{"echo":{message}}}"#);
        write!(output, "Content-Length: {}\r\n\r\n{answer}", answer.len()).expect("writing");
        output.flush().expect("flushing");
    }
}
