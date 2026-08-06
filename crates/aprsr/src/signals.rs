//! Operating-system signals, and what aprsr does about them.
//!
//! aprsr runs on Linux, macOS and Windows, and the three do not agree on how a service is
//! asked to stop. This module is the one place that knows the difference: it translates
//! whatever the platform sends into a [`SignalAction`], and everything above it deals only
//! in actions.
//!
//! The variants are deliberately compiled per platform rather than declared everywhere and
//! left unused on most of them. There is no `SIGTERM` on Windows and no Ctrl-Break on
//! Linux, and a type that claims otherwise invites handling a case that cannot occur.
//!
//! The translation itself is a pure function ([`Signal::action`]) so it can be tested
//! without raising real signals at the process running the test suite.

use std::fmt;

/// What aprsr should do in response to a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignalAction {
    /// Stop accepting, say goodbye to connected clients, and exit.
    Shutdown,
}

/// A signal aprsr recognises on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// Unix `SIGINT`, or Windows Ctrl-C. Interactive interrupt, on every platform.
    Interrupt,
    /// `SIGTERM` — the polite stop request an init system sends.
    #[cfg(unix)]
    Terminate,
    /// Ctrl-Break, which a console sends independently of Ctrl-C.
    #[cfg(windows)]
    Break,
    /// Console close.
    ///
    /// Windows allows only a few seconds after this before terminating the process, which
    /// is why the farewell path has to be prompt rather than leisurely.
    #[cfg(windows)]
    Close,
    /// System shutdown or user logoff — the nearest equivalent to `SIGTERM`.
    #[cfg(windows)]
    Shutdown,
}

impl Signal {
    /// What this signal means for the server.
    ///
    /// Every signal aprsr listens for currently means "stop". The mapping is written out
    /// rather than assumed so that adding one which means something else — a reload, say —
    /// cannot be done without deciding what it maps to.
    #[must_use]
    pub(crate) const fn action(self) -> SignalAction {
        match self {
            Self::Interrupt => SignalAction::Shutdown,
            #[cfg(unix)]
            Self::Terminate => SignalAction::Shutdown,
            #[cfg(windows)]
            Self::Break | Self::Close | Self::Shutdown => SignalAction::Shutdown,
        }
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // These names reach the operator in a log line, so each is the name that
        // platform's own documentation uses.
        let name = match self {
            Self::Interrupt => {
                if cfg!(windows) {
                    "Ctrl-C"
                } else {
                    "SIGINT"
                }
            }
            #[cfg(unix)]
            Self::Terminate => "SIGTERM",
            #[cfg(windows)]
            Self::Break => "Ctrl-Break",
            #[cfg(windows)]
            Self::Close => "console close",
            #[cfg(windows)]
            Self::Shutdown => "system shutdown",
        };
        f.write_str(name)
    }
}

/// Wait for the first signal that asks aprsr to stop.
///
/// Returns the signal that arrived, or `None` if none can be listened for at all — which
/// is not a reason to refuse to run, only a reason to say so.
#[cfg(unix)]
pub(crate) async fn next_shutdown_signal() -> Option<Signal> {
    use tokio::signal::unix::{SignalKind, signal};

    // SIGTERM is what an init system sends, so failing to register for it is worth
    // reporting: the process would still stop, but only ungracefully.
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, "could not listen for SIGTERM; only SIGINT will stop aprsr");
            tokio::signal::ctrl_c().await.ok()?;
            return Some(Signal::Interrupt);
        }
    };

    tokio::select! {
        result = tokio::signal::ctrl_c() => result.ok().map(|()| Signal::Interrupt),
        _ = terminate.recv() => Some(Signal::Terminate),
    }
}

/// Wait for the first console control event that asks aprsr to stop.
///
/// Ctrl-C alone is not enough here. A service host stopping aprsr sends
/// `CTRL_CLOSE_EVENT` or `CTRL_SHUTDOWN_EVENT`, neither of which is Ctrl-C, so listening
/// only for Ctrl-C would mean the process is killed without ever running its shutdown
/// path: connected clients would see the socket vanish rather than receive a farewell.
#[cfg(windows)]
pub(crate) async fn next_shutdown_signal() -> Option<Signal> {
    use tokio::signal::windows;

    let mut break_event = windows::ctrl_break().ok()?;
    let mut close = windows::ctrl_close().ok()?;
    let mut shutdown = windows::ctrl_shutdown().ok()?;

    tokio::select! {
        result = tokio::signal::ctrl_c() => result.ok().map(|()| Signal::Interrupt),
        _ = break_event.recv() => Some(Signal::Break),
        _ = close.recv() => Some(Signal::Close),
        _ = shutdown.recv() => Some(Signal::Shutdown),
    }
}

/// Anywhere else, Ctrl-C is all that can be relied on.
#[cfg(not(any(unix, windows)))]
pub(crate) async fn next_shutdown_signal() -> Option<Signal> {
    tokio::signal::ctrl_c().await.ok()?;
    Some(Signal::Interrupt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// Interrupt exists everywhere, so this case is the one that runs on every platform.
    #[rstest]
    #[case(Signal::Interrupt, SignalAction::Shutdown)]
    #[cfg_attr(unix, case(Signal::Terminate, SignalAction::Shutdown))]
    #[cfg_attr(windows, case(Signal::Break, SignalAction::Shutdown))]
    #[cfg_attr(windows, case(Signal::Close, SignalAction::Shutdown))]
    #[cfg_attr(windows, case(Signal::Shutdown, SignalAction::Shutdown))]
    fn signals_map_to_actions(#[case] signal: Signal, #[case] expected: SignalAction) {
        assert_eq!(signal.action(), expected);
    }

    /// An operator reading the log should see the name their platform's documentation
    /// uses, not a name aprsr invented.
    #[cfg(unix)]
    #[rstest]
    #[case(Signal::Interrupt, "SIGINT")]
    #[case(Signal::Terminate, "SIGTERM")]
    fn unix_signals_are_named_the_way_operators_name_them(
        #[case] signal: Signal,
        #[case] expected: &str,
    ) {
        assert_eq!(signal.to_string(), expected);
    }

    #[cfg(windows)]
    #[rstest]
    #[case(Signal::Interrupt, "Ctrl-C")]
    #[case(Signal::Break, "Ctrl-Break")]
    #[case(Signal::Close, "console close")]
    #[case(Signal::Shutdown, "system shutdown")]
    fn windows_events_are_named_the_way_operators_name_them(
        #[case] signal: Signal,
        #[case] expected: &str,
    ) {
        assert_eq!(signal.to_string(), expected);
    }
}
