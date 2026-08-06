//! Server-sent event framing.
//!
//! The dashboard used to poll three HTML fragments every few seconds, which is both later
//! than it needs to be and more requests than it needs to make. Server-sent events replace
//! that with one long-lived connection per viewer.
//!
//! This module is deliberately only the *wire format*. Producing the events lives in
//! [`crate::routes`], and the encoding here is a pure function over borrowed data so that
//! every rule of the format — the ones that are easy to get wrong and impossible to notice
//! until a browser silently ignores an event — can be tested without a socket, a runtime or
//! a client.
//!
//! The format is defined by the HTML standard's server-sent events section
//! (<https://html.spec.whatwg.org/multipage/server-sent-events.html>). Three of its rules
//! do real work here:
//!
//! * A `data` field cannot contain a newline. Multi-line payloads are sent as several
//!   consecutive `data:` lines, which the client rejoins with newlines between them.
//! * An event is terminated by a **blank line**. Without it the client buffers indefinitely
//!   and nothing is ever dispatched — the failure mode is silence, not an error.
//! * A line beginning with `:` is a comment and is discarded. That is what makes a
//!   heartbeat possible: it keeps the connection warm without dispatching an event.
//!
//! Hand-written rather than taken from `actix-web-lab`, which is explicitly experimental
//! and moves. `crates/aprsr-server/src/codec.rs` already sets the precedent: a small,
//! owned, tested implementation beats a dependency whose failure semantics are not quite
//! the ones wanted.

use std::fmt::Write as _;
use std::time::Duration;

/// One server-sent event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Event<'a> {
    /// The event name a browser listener matches on. `None` sends an unnamed event, which
    /// arrives at an `onmessage` handler.
    pub name: Option<&'a str>,
    /// The payload. May contain newlines; they are split across `data:` lines.
    pub data: &'a str,
    /// Opaque identifier. A client sends the last one it saw back in `Last-Event-ID` when
    /// it reconnects, so this is what makes a resumable stream possible.
    pub id: Option<u64>,
    /// How long a client should wait before reconnecting. Sent once at the start of a
    /// stream rather than on every event.
    pub retry: Option<Duration>,
}

impl<'a> Event<'a> {
    /// A named event carrying `data`.
    #[must_use]
    pub fn named(name: &'a str, data: &'a str) -> Self {
        Self {
            name: Some(name),
            data,
            ..Self::default()
        }
    }

    /// Attach an identifier, so a reconnecting client can say where it left off.
    #[must_use]
    pub const fn with_id(mut self, id: u64) -> Self {
        self.id = Some(id);
        self
    }

    /// Attach a reconnection delay.
    #[must_use]
    pub const fn with_retry(mut self, retry: Duration) -> Self {
        self.retry = Some(retry);
        self
    }

    /// Render to the wire format, terminating blank line included.
    #[must_use]
    pub fn encode(&self) -> String {
        // Roughly the payload plus the field names; one allocation for the common case.
        let mut out = String::with_capacity(self.data.len() + 32);

        if let Some(retry) = self.retry {
            // Milliseconds, per the standard. `as_millis` is u128; a retry longer than a
            // u64 of milliseconds is nonsense, and saturating beats wrapping to something
            // tiny that would make a client reconnect in a hot loop.
            let millis = u64::try_from(retry.as_millis()).unwrap_or(u64::MAX);
            let _ = writeln!(out, "retry: {millis}");
        }
        if let Some(id) = self.id {
            let _ = writeln!(out, "id: {id}");
        }
        if let Some(name) = self.name {
            let _ = writeln!(out, "event: {name}");
        }

        // A `data` field cannot span lines, so each line of the payload becomes its own
        // field and the client rejoins them.
        //
        // The standard's parser treats CRLF, a lone CR and a lone LF as equivalent line
        // terminators, so all three have to be split on here. Stripping only a *trailing*
        // CR is not enough and is the subtle version of this bug: a bare CR in the middle
        // of a payload would end the `data` field early, and everything after it on that
        // line would be reinterpreted as a new field name. A property test covers this.
        //
        // `split` rather than `lines`, because `lines` discards a trailing empty line and
        // the client would rebuild a payload one newline shorter than the one sent.
        for line in split_lines(self.data) {
            let _ = writeln!(out, "data: {line}");
        }

        // The blank line is what dispatches the event. Omitting it produces a client that
        // waits forever with no error anywhere — worth its own comment and its own test.
        out.push('\n');
        out
    }
}

/// Split a payload the way a server-sent-events client will.
///
/// CRLF, a lone CR and a lone LF are all line terminators, and a CRLF pair counts as one
/// rather than two. Borrowing rather than allocating because this runs once per event on
/// the packet stream, where a busy feed is hundreds a second and almost none of them
/// contain a line break at all.
fn split_lines(data: &str) -> SplitLines<'_> {
    SplitLines { rest: Some(data) }
}

struct SplitLines<'a> {
    /// `None` once the final line has been yielded, which is what distinguishes "finished"
    /// from "the last line was empty" — `"a\n"` must yield `["a", ""]`, not `["a"]`.
    rest: Option<&'a str>,
}

impl<'a> Iterator for SplitLines<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let rest = self.rest?;
        let Some(index) = rest.find(['\r', '\n']) else {
            // No terminator left: this is the final line.
            self.rest = None;
            return Some(rest);
        };

        let (line, tail) = rest.split_at(index);
        // The terminator is one ASCII byte, or two for CRLF, so slicing past it always
        // lands on a character boundary.
        let tail = tail
            .strip_prefix("\r\n")
            .or_else(|| tail.get(1..))
            .unwrap_or_default();
        self.rest = Some(tail);
        Some(line)
    }
}

/// A comment frame, used to keep an idle connection open.
///
/// Proxies and load balancers reap connections that go quiet, and a status stream is quiet
/// by nature whenever nothing is happening. A comment costs three bytes, is discarded by
/// the client, and does not dispatch an event — so a heartbeat cannot be mistaken for data.
#[must_use]
pub fn heartbeat() -> String {
    ":\n\n".to_owned()
}

/// The content type a server-sent event stream must be served with.
pub const CONTENT_TYPE: &str = "text/event-stream";

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn a_named_event_carries_its_name_and_data() {
        let encoded = Event::named("status", "{\"clients\":3}").encode();
        assert_eq!(encoded, "event: status\ndata: {\"clients\":3}\n\n");
    }

    #[test]
    fn an_unnamed_event_omits_the_event_field() {
        let encoded = Event {
            data: "plain",
            ..Event::default()
        }
        .encode();
        assert_eq!(encoded, "data: plain\n\n");
    }

    /// The rule most likely to be got wrong, because a single `data:` line containing a
    /// newline produces a frame the client silently misparses rather than rejecting.
    #[test]
    fn a_multi_line_payload_becomes_one_data_field_per_line() {
        let encoded = Event::named("packet", "first\nsecond\nthird").encode();
        assert_eq!(
            encoded,
            "event: packet\ndata: first\ndata: second\ndata: third\n\n"
        );
    }

    /// `lines()` would discard this trailing empty line and the client would rebuild a
    /// payload one newline shorter than the one that was sent.
    #[test]
    fn a_trailing_newline_in_the_payload_is_preserved() {
        let encoded = Event::named("x", "body\n").encode();
        assert_eq!(encoded, "event: x\ndata: body\ndata: \n\n");
    }

    /// All three terminators are equivalent to a client, and CRLF is *one* of them rather
    /// than two — otherwise every Windows-authored payload would gain a blank line.
    #[rstest]
    #[case("one\r\ntwo", "data: one\ndata: two\n")] // CRLF is a single terminator
    #[case("one\ntwo", "data: one\ndata: two\n")]
    #[case("one\rtwo", "data: one\ndata: two\n")] // a lone CR terminates too
    #[case("one\r\ntwo\r", "data: one\ndata: two\ndata: \n")] // trailing CR ends a line
    fn every_line_terminator_is_handled(#[case] data: &str, #[case] expected_data: &str) {
        let encoded = Event {
            data,
            ..Event::default()
        }
        .encode();
        assert_eq!(encoded, format!("{expected_data}\n"));
    }

    /// The bug a property test found: stripping only a *trailing* carriage return leaves a
    /// bare CR in the middle of a payload, where it ends the `data` field early and makes
    /// everything after it on that line look like a new field name to the client.
    #[test]
    fn a_carriage_return_in_the_middle_of_a_payload_does_not_corrupt_the_frame() {
        let encoded = Event::named("x", "\r0").encode();
        assert_eq!(encoded, "event: x\ndata: \ndata: 0\n\n");
    }

    #[test]
    fn an_empty_payload_still_produces_a_valid_frame() {
        let encoded = Event::named("tick", "").encode();
        assert_eq!(encoded, "event: tick\ndata: \n\n");
    }

    /// Fields must come in this order for the id to apply to the event that follows.
    #[test]
    fn fields_are_ordered_retry_then_id_then_event_then_data() {
        let encoded = Event::named("status", "body")
            .with_id(42)
            .with_retry(Duration::from_secs(5))
            .encode();
        assert_eq!(
            encoded,
            "retry: 5000\nid: 42\nevent: status\ndata: body\n\n"
        );
    }

    /// Every frame ends with a blank line; without it nothing is ever dispatched and the
    /// failure is silent at both ends.
    #[rstest]
    #[case(Event::named("a", "b"))]
    #[case(Event { data: "b", ..Event::default() })]
    #[case(Event::named("a", "multi\nline"))]
    #[case(Event::named("a", ""))]
    fn every_frame_is_terminated_by_a_blank_line(#[case] event: Event<'_>) {
        assert!(event.encode().ends_with("\n\n"), "{:?}", event.encode());
    }

    /// A heartbeat is a comment: it keeps the connection warm without dispatching anything,
    /// so a client cannot mistake it for data.
    #[test]
    fn a_heartbeat_is_a_comment_and_dispatches_nothing() {
        let beat = heartbeat();
        assert!(beat.starts_with(':'));
        assert!(beat.ends_with("\n\n"));
        assert!(
            !beat.contains("data:"),
            "a heartbeat must not look like an event"
        );
    }

    /// An absurd retry must not wrap to a tiny value, which would put a client into a
    /// reconnect loop against the server that sent it.
    #[test]
    fn an_implausible_retry_saturates_rather_than_wrapping() {
        let encoded = Event::named("x", "y")
            .with_retry(Duration::from_secs(u64::MAX))
            .encode();
        assert!(encoded.starts_with(&format!("retry: {}\n", u64::MAX)));
    }

    proptest::proptest! {
        /// Whatever the payload, the frame must stay well-formed: terminated by a blank
        /// line, and with no `data` field containing an embedded newline. Payloads here
        /// come from packets and JSON, neither of which this module gets to constrain.
        #[test]
        fn frames_stay_well_formed(data in "[ -~\n\r]{0,200}") {
            let encoded = Event::named("x", &data).encode();
            proptest::prop_assert!(encoded.ends_with("\n\n"));
            for line in encoded.lines() {
                if let Some(field) = line.strip_prefix("data: ") {
                    proptest::prop_assert!(!field.contains('\n'));
                    proptest::prop_assert!(!field.contains('\r'));
                }
            }
        }
    }
}
