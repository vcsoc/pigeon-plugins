//! Line transport: bounded line reading and the outbound queue with a writer thread.
//!
//! The reader thread never waits on the outbound queue (control messages are unbounded), so a
//! `pong` is always sent promptly. Normal messages are bounded; a producer that outruns the host
//! blocks, which is back-pressure on the producing thread, never on the protocol thread.

use std::collections::VecDeque;
use std::io::{self, BufRead, Write};
use std::sync::{Condvar, Mutex};

use crate::protocol::MAX_LINE_BYTES;

/// Why a line could not be read.
#[derive(Debug)]
pub enum LineError {
    /// The line was longer than [`MAX_LINE_BYTES`]; it has been skipped up to its newline.
    TooLong(usize),
    /// End of input.
    Eof,
    /// An I/O error.
    Io(io::Error),
}

/// Read one line of at most `max` bytes (without the newline). Over-long lines are consumed up to
/// the newline and reported as [`LineError::TooLong`], so the stream stays in sync.
pub fn read_line_bounded<R: BufRead>(reader: &mut R, max: usize) -> Result<String, LineError> {
    let mut line: Vec<u8> = Vec::new();
    let mut too_long = false;
    let mut total = 0usize;
    loop {
        let (done, used) = {
            let buf = match reader.fill_buf() {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(LineError::Io(e)),
            };
            if buf.is_empty() {
                if total == 0 {
                    return Err(LineError::Eof);
                }
                (true, 0)
            } else {
                match buf.iter().position(|b| *b == b'\n') {
                    Some(i) => {
                        total += i;
                        if !too_long {
                            if line.len() + i > max {
                                too_long = true;
                            } else {
                                line.extend_from_slice(&buf[..i]);
                            }
                        }
                        (true, i + 1)
                    }
                    None => {
                        total += buf.len();
                        if !too_long {
                            if line.len() + buf.len() > max {
                                too_long = true;
                                line.clear();
                            } else {
                                line.extend_from_slice(buf);
                            }
                        }
                        (false, buf.len())
                    }
                }
            }
        };
        reader.consume(used);
        if done {
            break;
        }
    }
    if too_long {
        return Err(LineError::TooLong(total));
    }
    // Tolerate CRLF.
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).map_err(|_| LineError::TooLong(0))
}

#[derive(Default)]
struct OutState {
    control: VecDeque<String>,
    normal: VecDeque<String>,
    closed: bool,
}

/// The outbound queue. Control lines (pong, responses) never block; normal lines block the
/// producer when `capacity` is reached.
pub struct Outbox {
    state: Mutex<OutState>,
    not_empty: Condvar,
    not_full: Condvar,
    capacity: usize,
}

impl Outbox {
    pub fn new(capacity: usize) -> Outbox {
        Outbox {
            state: Mutex::new(OutState::default()),
            not_empty: Condvar::new(),
            not_full: Condvar::new(),
            capacity: capacity.max(1),
        }
    }

    /// Enqueue a control line. Never blocks. Returns false when the transport is closed.
    pub fn send_control(&self, line: String) -> bool {
        let mut st = lock(&self.state);
        if st.closed {
            return false;
        }
        st.control.push_back(line);
        self.not_empty.notify_one();
        true
    }

    /// Enqueue a normal line, blocking while the queue is full. Returns false when closed.
    pub fn send(&self, line: String) -> bool {
        let mut st = lock(&self.state);
        while !st.closed && st.normal.len() >= self.capacity {
            st = match self.not_full.wait(st) {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
        }
        if st.closed {
            return false;
        }
        st.normal.push_back(line);
        self.not_empty.notify_one();
        true
    }

    /// Next line to write: control first. Blocks until a line is available; None once closed and empty.
    pub fn next(&self) -> Option<String> {
        let mut st = lock(&self.state);
        loop {
            if let Some(line) = st.control.pop_front() {
                return Some(line);
            }
            if let Some(line) = st.normal.pop_front() {
                self.not_full.notify_one();
                return Some(line);
            }
            if st.closed {
                return None;
            }
            st = match self.not_empty.wait(st) {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
        }
    }

    /// Stop accepting lines and wake every waiter.
    pub fn close(&self) {
        let mut st = lock(&self.state);
        st.closed = true;
        self.not_empty.notify_all();
        self.not_full.notify_all();
    }

    /// Lines currently queued (normal and control).
    pub fn pending(&self) -> usize {
        let st = lock(&self.state);
        st.control.len() + st.normal.len()
    }
}

/// Run the writer loop: write each queued line with its newline and flush. Returns when the outbox
/// is closed or the writer fails (in which case the outbox is closed too).
pub fn write_loop<W: Write>(outbox: &Outbox, mut out: W) {
    while let Some(line) = outbox.next() {
        let ok = out
            .write_all(line.as_bytes())
            .and_then(|_| out.write_all(b"\n"))
            .and_then(|_| out.flush());
        if ok.is_err() {
            outbox.close();
            break;
        }
    }
    let _ = out.flush();
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// The maximum line size, re-exported for transports.
pub const MAX_LINE: usize = MAX_LINE_BYTES;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_lines_and_handles_crlf_and_eof() {
        let mut r = Cursor::new(b"one\r\ntwo\nthree".to_vec());
        assert_eq!(read_line_bounded(&mut r, 100).unwrap(), "one");
        assert_eq!(read_line_bounded(&mut r, 100).unwrap(), "two");
        assert_eq!(read_line_bounded(&mut r, 100).unwrap(), "three");
        assert!(matches!(
            read_line_bounded(&mut r, 100),
            Err(LineError::Eof)
        ));
    }

    #[test]
    fn over_long_line_is_skipped_and_stream_stays_in_sync() {
        let mut data = vec![b'x'; 50];
        data.push(b'\n');
        data.extend_from_slice(b"ok\n");
        let mut r = Cursor::new(data);
        assert!(matches!(
            read_line_bounded(&mut r, 10),
            Err(LineError::TooLong(_))
        ));
        assert_eq!(read_line_bounded(&mut r, 10).unwrap(), "ok");
    }

    #[test]
    fn control_lines_overtake_queued_normal_lines() {
        let outbox = Outbox::new(8);
        outbox.send("normal-1".into());
        outbox.send("normal-2".into());
        outbox.send_control("pong".into());
        assert_eq!(outbox.next().unwrap(), "pong");
        assert_eq!(outbox.next().unwrap(), "normal-1");
        outbox.close();
        assert_eq!(outbox.next().unwrap(), "normal-2");
        assert!(outbox.next().is_none());
        assert!(!outbox.send("late".into()));
    }

    #[test]
    fn writer_writes_newline_terminated_lines() {
        let outbox = Outbox::new(8);
        outbox.send("a".into());
        outbox.send_control("b".into());
        outbox.close();
        let mut sink: Vec<u8> = Vec::new();
        write_loop(&outbox, &mut sink);
        assert_eq!(String::from_utf8(sink).unwrap(), "b\na\n");
    }
}
