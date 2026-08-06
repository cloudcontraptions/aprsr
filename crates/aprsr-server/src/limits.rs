//! Process resource limits.
//!
//! An APRS-IS server holds one file descriptor per connected client, so the descriptor
//! limit is effectively the client limit. Most Linux distributions still ship a soft limit
//! of 1024, which a server configured for ten thousand clients will hit long before it
//! reaches its configured cap — and the symptom is `accept` failing with "too many open
//! files" rather than anything that points at the real cause.
//!
//! `limits.file_limit` has been in the configuration schema since the beginning and was
//! never applied to anything. This module applies it.
//!
//! The decision of *what* to ask for is a pure function ([`desired_limit`]) so it can be
//! tested; only the asking is platform-specific.

/// What happened when the limit was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileLimit {
    /// The soft limit was raised to this value.
    Raised { from: u64, to: u64 },
    /// The soft limit was already at least the configured value; nothing to do.
    AlreadySufficient { current: u64 },
    /// The hard limit is lower than the configuration asked for, so this is all that could
    /// be had. Running as root, or an adjusted `/etc/security/limits.conf`, would give
    /// more.
    CappedByHardLimit { requested: u64, granted: u64 },
    /// This platform has no equivalent limit to raise.
    NotApplicable,
    /// The limit could not be read or changed. Not fatal: the server runs, and will fail
    /// later if it genuinely runs out.
    Failed { reason: String },
}

/// Decide what soft limit to request.
///
/// Returns `None` when the current soft limit is already sufficient — asking for a limit
/// that is already in force is a syscall that can only fail, never help.
///
/// The request is clamped to the hard limit because a request above it is refused
/// outright, which would leave the soft limit at its original value. Asking for the most
/// that can be granted is more useful than asking for the ideal and getting nothing.
#[must_use]
pub fn desired_limit(configured: u64, soft: u64, hard: u64) -> Option<u64> {
    if soft >= configured {
        return None;
    }
    Some(configured.min(hard))
}

/// Raise the open-file-descriptor limit toward `configured`.
///
/// Never fails the caller: a server that cannot raise its limit should still start and say
/// so, because the limit only matters once enough clients connect to reach it.
#[cfg(unix)]
#[must_use]
pub fn apply_file_limit(configured: u64) -> FileLimit {
    let (soft, hard) = match rlimit::Resource::NOFILE.get() {
        Ok(pair) => pair,
        Err(error) => {
            return FileLimit::Failed {
                reason: error.to_string(),
            };
        }
    };

    let Some(target) = desired_limit(configured, soft, hard) else {
        return FileLimit::AlreadySufficient { current: soft };
    };

    // The hard limit is left where it is. Raising it needs privileges aprsr should not
    // assume it has, and lowering it would be irreversible for the life of the process.
    if let Err(error) = rlimit::Resource::NOFILE.set(target, hard) {
        return FileLimit::Failed {
            reason: error.to_string(),
        };
    }

    if target < configured {
        FileLimit::CappedByHardLimit {
            requested: configured,
            granted: target,
        }
    } else {
        FileLimit::Raised {
            from: soft,
            to: target,
        }
    }
}

/// Windows has no per-process socket limit to raise.
///
/// `_setmaxstdio` governs the C runtime's stdio table, not sockets, and does not apply
/// here. The number of concurrent sockets is bounded by non-paged pool and by ephemeral
/// port exhaustion, neither of which is a per-process dial. The configured value is still
/// validated by `aprsr-config`, so a nonsensical one is still rejected; it simply has
/// nothing to be applied to.
#[cfg(not(unix))]
#[must_use]
pub fn apply_file_limit(_configured: u64) -> FileLimit {
    FileLimit::NotApplicable
}

/// Apply the limit and report the outcome to the log.
pub fn apply_and_report(configured: u64) {
    match apply_file_limit(configured) {
        FileLimit::Raised { from, to } => {
            tracing::info!(from, to, "raised the open file limit");
        }
        FileLimit::AlreadySufficient { current } => {
            tracing::debug!(current, "the open file limit is already sufficient");
        }
        FileLimit::CappedByHardLimit { requested, granted } => {
            tracing::warn!(
                requested,
                granted,
                "the open file limit was capped by the hard limit; \
                 aprsr will refuse connections above roughly this many clients"
            );
        }
        FileLimit::NotApplicable => {
            tracing::debug!(
                "limits.file_limit does not apply on this platform and was not changed"
            );
        }
        FileLimit::Failed { reason } => {
            tracing::warn!(
                %reason,
                "could not adjust the open file limit; \
                 aprsr will start anyway and may run out of descriptors under load"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    // Already at or above what was asked for: nothing to request.
    #[case(1000, 1000, 4096, None)]
    #[case(1000, 2000, 4096, None)]
    // Below it, with headroom under the hard limit: ask for exactly what was configured.
    #[case(10_000, 1024, 1_048_576, Some(10_000))]
    #[case(10_000, 1024, 10_000, Some(10_000))]
    // The hard limit is the ceiling. Asking for more than it fails outright and leaves the
    // soft limit untouched, so ask for the most that can actually be granted instead.
    #[case(10_000, 1024, 4096, Some(4096))]
    // A hard limit below even the current soft limit should not produce an increase.
    #[case(10_000, 8192, 4096, Some(4096))]
    fn requests_the_most_that_can_be_granted(
        #[case] configured: u64,
        #[case] soft: u64,
        #[case] hard: u64,
        #[case] expected: Option<u64>,
    ) {
        assert_eq!(desired_limit(configured, soft, hard), expected);
    }

    /// The whole point of the setting is a server that expects many clients, so the
    /// shipped default must actually ask for something on a typical distribution.
    #[test]
    fn the_default_configuration_raises_a_typical_distribution_limit() {
        assert_eq!(desired_limit(10_000, 1024, 1_048_576), Some(10_000));
    }

    /// Applying the limit must never panic or abort startup, whatever the platform says.
    #[test]
    fn applying_the_limit_reports_rather_than_failing() {
        // 64 is below any plausible soft limit, so this exercises the "already
        // sufficient" path on Unix and the "not applicable" path elsewhere. Either way it
        // must return rather than panic, and must not lower anything.
        let outcome = apply_file_limit(64);
        assert!(matches!(
            outcome,
            FileLimit::AlreadySufficient { .. }
                | FileLimit::NotApplicable
                | FileLimit::Failed { .. }
        ));
    }
}
