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
    /// Re-read the configuration file without dropping anyone.
    Reload,
}

/// A signal aprsr recognises on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// Unix `SIGINT`, or Windows Ctrl-C. Interactive interrupt, on every platform.
    Interrupt,
    /// `SIGTERM` — the polite stop request an init system sends.
    #[cfg(unix)]
    Terminate,
    /// `SIGHUP` — conventionally "re-read your configuration".
    ///
    /// Windows has no equivalent, which is why the reload capability is also reachable
    /// over HTTP: that path works identically on all three platforms.
    #[cfg(unix)]
    Hangup,
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
    /// Written out per signal rather than defaulted, so a signal cannot be added without
    /// deciding what it does.
    #[must_use]
    pub(crate) const fn action(self) -> SignalAction {
        match self {
            Self::Interrupt => SignalAction::Shutdown,
            #[cfg(unix)]
            Self::Terminate => SignalAction::Shutdown,
            #[cfg(unix)]
            Self::Hangup => SignalAction::Reload,
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
            #[cfg(unix)]
            Self::Hangup => "SIGHUP",
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

/// A source of signals that can be awaited repeatedly.
///
/// Repeatedly matters: a reload does not end the process, so the watcher goes back to
/// waiting afterwards. Registering the handlers once and reusing them avoids re-registering
/// on every iteration, which would drop signals arriving in the gap.
#[cfg(unix)]
pub(crate) struct Signals {
    terminate: Option<tokio::signal::unix::Signal>,
    hangup: Option<tokio::signal::unix::Signal>,
}

#[cfg(unix)]
impl Signals {
    /// Register the handlers. Never fails: a signal that cannot be registered for is
    /// reported and then simply never arrives, which is better than refusing to start.
    pub(crate) fn install() -> Self {
        use tokio::signal::unix::{SignalKind, signal};

        // SIGTERM is what an init system sends, so failing to register for it is worth a
        // warning: the process would still stop, but only ungracefully.
        let terminate = signal(SignalKind::terminate())
            .inspect_err(|error| {
                tracing::warn!(%error, "could not listen for SIGTERM; only SIGINT will stop aprsr");
            })
            .ok();

        let hangup = signal(SignalKind::hangup())
            .inspect_err(|error| {
                tracing::warn!(%error, "could not listen for SIGHUP; reload by other means");
            })
            .ok();

        Self { terminate, hangup }
    }

    /// Wait for the next signal aprsr acts on.
    ///
    /// Returns `None` only when nothing can be waited for at all.
    pub(crate) async fn next(&mut self) -> Option<Signal> {
        // `recv` on an absent handler must never resolve, or the select would spin. A
        // pending future is the honest representation of "this signal cannot arrive".
        let terminate = async {
            match self.terminate.as_mut() {
                Some(stream) => stream.recv().await,
                None => std::future::pending().await,
            }
        };
        let hangup = async {
            match self.hangup.as_mut() {
                Some(stream) => stream.recv().await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            result = tokio::signal::ctrl_c() => result.ok().map(|()| Signal::Interrupt),
            _ = terminate => Some(Signal::Terminate),
            _ = hangup => Some(Signal::Hangup),
        }
    }
}

/// Console control events, awaited repeatedly.
///
/// Ctrl-C alone is not enough here. A service host stopping aprsr sends
/// `CTRL_CLOSE_EVENT` or `CTRL_SHUTDOWN_EVENT`, neither of which is Ctrl-C, so listening
/// only for Ctrl-C would mean the process is killed without ever running its shutdown
/// path: connected clients would see the socket vanish rather than receive a farewell.
///
/// There is no console event meaning "reload"; that capability is reached over HTTP.
#[cfg(windows)]
pub(crate) struct Signals {
    break_event: Option<tokio::signal::windows::CtrlBreak>,
    close: Option<tokio::signal::windows::CtrlClose>,
    shutdown: Option<tokio::signal::windows::CtrlShutdown>,
}

#[cfg(windows)]
impl Signals {
    pub(crate) fn install() -> Self {
        use tokio::signal::windows;

        Self {
            break_event: windows::ctrl_break().ok(),
            close: windows::ctrl_close().ok(),
            shutdown: windows::ctrl_shutdown().ok(),
        }
    }

    pub(crate) async fn next(&mut self) -> Option<Signal> {
        macro_rules! event {
            ($field:expr) => {
                async {
                    match $field.as_mut() {
                        Some(stream) => stream.recv().await,
                        None => std::future::pending().await,
                    }
                }
            };
        }

        tokio::select! {
            result = tokio::signal::ctrl_c() => result.ok().map(|()| Signal::Interrupt),
            _ = event!(self.break_event) => Some(Signal::Break),
            _ = event!(self.close) => Some(Signal::Close),
            _ = event!(self.shutdown) => Some(Signal::Shutdown),
        }
    }
}

/// Anywhere else, Ctrl-C is all that can be relied on.
#[cfg(not(any(unix, windows)))]
pub(crate) struct Signals;

#[cfg(not(any(unix, windows)))]
impl Signals {
    pub(crate) fn install() -> Self {
        Self
    }

    pub(crate) async fn next(&mut self) -> Option<Signal> {
        tokio::signal::ctrl_c().await.ok()?;
        Some(Signal::Interrupt)
    }
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
