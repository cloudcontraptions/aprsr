//! Re-reading configuration without dropping clients.
//!
//! aprsc reconfigures on `SIGUSR1` and can even re-execute itself into a new binary while
//! holding its connections open. aprsr does not reproduce those mechanics — they are deeply
//! Unix-specific and a container restart covers most of what they were for — but the
//! *capability* an operator actually wants from them is worth having: change a setting and
//! have it take effect without disconnecting several thousand clients.
//!
//! Not everything can be adopted by a running process. A listener that is already bound
//! cannot move to a different address, and the server's own callsign is woven into the q
//! construct of every packet in flight. Rather than silently ignoring those, a reload
//! reports them: [`ReloadReport::requires_restart`] names each setting that was changed in
//! the file but is still running with its old value.
//!
//! The comparison is a pure function over two [`Config`] values so that the classification
//! — which is the part that is easy to get quietly wrong — is testable without a server, a
//! socket or a signal.

use aprsr_config::Config;

/// What a reload did, and what it could not do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReloadReport {
    /// Settings that changed and are now in force.
    pub applied: Vec<Change>,
    /// Settings that changed in the file but cannot be adopted while running. The server
    /// keeps its current value for these.
    pub requires_restart: Vec<Change>,
}

impl ReloadReport {
    /// Whether anything at all differed from the running configuration.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.applied.is_empty() && self.requires_restart.is_empty()
    }

    /// Whether the operator needs to restart for the file to be fully in force.
    #[must_use]
    pub fn needs_restart(&self) -> bool {
        !self.requires_restart.is_empty()
    }
}

/// One setting that differs between the running configuration and the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Dotted path of the setting, as it appears in the TOML — `limits.client_timeout`.
    pub setting: &'static str,
    /// What the running server is using.
    pub from: String,
    /// What the file now says.
    pub to: String,
}

impl Change {
    /// Both values are taken as trait objects so a call site can pass whatever shape the
    /// setting naturally has — a `String`, a `&str`, a number, a `Path::display()` — without
    /// each one having to convert first.
    fn new(
        setting: &'static str,
        from: &dyn std::fmt::Display,
        to: &dyn std::fmt::Display,
    ) -> Self {
        Self {
            setting,
            from: from.to_string(),
            to: to.to_string(),
        }
    }
}

/// Compare the running configuration against a freshly loaded one.
///
/// A setting is *applicable* when the running server reads it from the current
/// configuration each time it is needed, rather than having captured it into something
/// already constructed. That is the whole rule, and each classification below says which
/// side of it the setting falls on and why.
#[must_use]
pub fn compare(current: &Config, new: &Config) -> ReloadReport {
    let mut report = ReloadReport::default();

    // --- applied ---------------------------------------------------------------------
    //
    // Read from the live configuration at the moment they are used.

    // Shown on the status page, read per request.
    if current.server.admin != new.server.admin {
        report.applied.push(Change::new(
            "server.admin",
            &unset_if_empty(&current.server.admin),
            &unset_if_empty(&new.server.admin),
        ));
    }
    if current.server.email != new.server.email {
        report.applied.push(Change::new(
            "server.email",
            &unset_if_empty(&current.server.email),
            &unset_if_empty(&new.server.email),
        ));
    }

    // Read when a connection is accepted, so these govern every client that connects from
    // now on. Clients already connected keep the timeout and keepalive period they were
    // admitted under, which is why the log says "new connections" rather than "clients".
    if current.limits.client_timeout != new.limits.client_timeout {
        report.applied.push(Change::new(
            "limits.client_timeout",
            &current.limits.client_timeout.as_secs(),
            &new.limits.client_timeout.as_secs(),
        ));
    }
    if current.limits.keepalive_interval != new.limits.keepalive_interval {
        report.applied.push(Change::new(
            "limits.keepalive_interval",
            &current.limits.keepalive_interval.as_secs(),
            &new.limits.keepalive_interval.as_secs(),
        ));
    }
    if current.limits.client_queue != new.limits.client_queue {
        report.applied.push(Change::new(
            "limits.client_queue",
            &current.limits.client_queue,
            &new.limits.client_queue,
        ));
    }

    // --- requires a restart ----------------------------------------------------------

    // The server's callsign is written into the q construct of every packet it relays and
    // is what other servers use to detect loops. Changing it underneath packets already in
    // flight would make this server briefly indistinguishable from a second one.
    if current.server.id != new.server.id {
        report
            .requires_restart
            .push(Change::new("server.id", &current.server.id, &new.server.id));
    }

    // Captured by the dispatch task when it starts, because re-reading it per packet would
    // put a lock on the hot path for a value that changes approximately never.
    if current.limits.dupecheck_window != new.limits.dupecheck_window {
        report.requires_restart.push(Change::new(
            "limits.dupecheck_window",
            &current.limits.dupecheck_window.as_secs(),
            &new.limits.dupecheck_window.as_secs(),
        ));
    }

    // The age of entries already in the gated-station table. Changing it under them would
    // make a live gating either expire early or outlive the setting that created it, so the
    // table is built with the window it will keep. See `heard::Heard::new`.
    if current.limits.heard_window != new.limits.heard_window {
        report.requires_restart.push(Change::new(
            "limits.heard_window",
            &current.limits.heard_window.as_secs(),
            &new.limits.heard_window.as_secs(),
        ));
    }

    // Applied once at startup before anything opens a descriptor, and a soft limit cannot
    // be lowered back down by an unprivileged process anyway.
    if current.limits.file_limit != new.limits.file_limit {
        report.requires_restart.push(Change::new(
            "limits.file_limit",
            &current.limits.file_limit,
            &new.limits.file_limit,
        ));
    }

    // The connection pool is open and holds the migrations that ran against it.
    if current.database.url != new.database.url {
        report.requires_restart.push(Change::new(
            "database.url",
            &current.database.url,
            &new.database.url,
        ));
    }

    // Sockets are already bound. Rebinding is exactly the disruption a reload exists to
    // avoid, so it is reported instead of attempted.
    if current.http.status_bind != new.http.status_bind {
        report.requires_restart.push(Change::new(
            "http.status_bind",
            &describe_bind(current.http.status_bind),
            &describe_bind(new.http.status_bind),
        ));
    }

    if current.server.run_dir != new.server.run_dir {
        report.requires_restart.push(Change::new(
            "server.run_dir",
            &current.server.run_dir.display(),
            &new.server.run_dir.display(),
        ));
    }

    compare_listeners(current, new, &mut report);

    report
}

/// Listeners are compared as a set, because adding, removing or moving one all mean the
/// same thing to a running server: a socket would have to be bound or closed.
fn compare_listeners(current: &Config, new: &Config, report: &mut ReloadReport) {
    if current.listeners.len() != new.listeners.len() {
        report.requires_restart.push(Change::new(
            "listen",
            &format!("{} listeners", current.listeners.len()),
            &format!("{} listeners", new.listeners.len()),
        ));
        return;
    }

    for (old, fresh) in current.listeners.iter().zip(&new.listeners) {
        if old.bind != fresh.bind || old.name != fresh.name || old.kind != fresh.kind {
            report.requires_restart.push(Change::new(
                "listen",
                &format!("{} on {}", old.name, old.bind),
                &format!("{} on {}", fresh.name, fresh.bind),
            ));
        } else if old.filter != fresh.filter || old.max_clients != fresh.max_clients {
            // The forced filter and the client cap are held in the `ListenerContext` that
            // was built when the socket was bound and handed to every connection task, so
            // they are not picked up by swapping the configuration alone.
            report.requires_restart.push(Change::new(
                "listen.filter",
                &old.filter.as_deref().unwrap_or("(none)"),
                &fresh.filter.as_deref().unwrap_or("(none)"),
            ));
        }
    }
}

fn describe_bind(bind: Option<std::net::SocketAddr>) -> String {
    bind.map_or_else(|| "(disabled)".to_owned(), |address| address.to_string())
}

/// These are empty strings rather than `Option`s in the schema; an empty one in a log line
/// reads as though something went wrong, so say what it means instead.
fn unset_if_empty(value: &str) -> &str {
    if value.is_empty() { "(unset)" } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a configuration from the parts each test varies, so the fixtures differ only
    /// in the setting under test.
    fn config_of(server: &str, limits: &str, listeners: &str) -> Config {
        let listeners = if listeners.is_empty() {
            r#"
[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:14580"
"#
        } else {
            listeners
        };
        Config::from_toml(&format!(
            r#"
[server]
id = "T2TEST"
{server}

[limits]
{limits}
{listeners}
"#
        ))
        .expect("valid test configuration")
    }

    fn base() -> Config {
        config_of("", "", "")
    }

    #[test]
    fn an_unchanged_configuration_reports_nothing() {
        let report = compare(&base(), &base());
        assert!(report.is_empty());
        assert!(!report.needs_restart());
    }

    /// Contact details are read per status request, so they take effect at once.
    #[test]
    fn operator_contact_details_are_applied() {
        let report = compare(&base(), &config_of(r#"admin = "Someone, N0CALL""#, "", ""));
        assert_eq!(report.applied.len(), 1);
        assert_eq!(
            report.applied.first().map(|c| c.setting),
            Some("server.admin")
        );
        assert_eq!(
            report.applied.first().map(|c| c.from.as_str()),
            Some("(unset)"),
            "an empty string reads as a mistake in a log line"
        );
        assert!(!report.needs_restart());
    }

    /// Timeouts are read when a connection is accepted, so they govern new clients.
    #[test]
    fn connection_timeouts_are_applied() {
        let report = compare(&base(), &config_of("", r#"client_timeout = "1h""#, ""));
        assert_eq!(
            report.applied.first().map(|c| c.setting),
            Some("limits.client_timeout")
        );
        assert!(!report.needs_restart());
    }

    /// The server's callsign is in the q construct of every packet in flight. Changing it
    /// under a running server would make it briefly look like a different server to the
    /// rest of the network, which is a loop-detection problem for everyone, not just here.
    #[test]
    fn the_server_id_requires_a_restart() {
        let mut other = base();
        other.server.id = "T2OTHER".to_owned();

        let report = compare(&base(), &other);
        assert!(report.applied.is_empty());
        assert_eq!(
            report.requires_restart.first().map(|c| c.setting),
            Some("server.id")
        );
        assert!(report.needs_restart());
    }

    /// The dupe window is captured by the dispatch task at startup rather than re-read per
    /// packet, so a reload cannot change it. Reporting that is the honest outcome.
    #[test]
    fn the_dupecheck_window_requires_a_restart() {
        let report = compare(&base(), &config_of("", r#"dupecheck_window = "45s""#, ""));
        assert_eq!(
            report.requires_restart.first().map(|c| c.setting),
            Some("limits.dupecheck_window")
        );
    }

    /// Moving a bound socket is precisely the disruption a reload exists to avoid.
    #[test]
    fn moving_a_listener_requires_a_restart() {
        let moved = config_of(
            "",
            "",
            r#"
[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:14581"
"#,
        );
        let report = compare(&base(), &moved);
        assert_eq!(
            report.requires_restart.first().map(|c| c.setting),
            Some("listen")
        );
    }

    #[test]
    fn adding_a_listener_requires_a_restart() {
        let added = config_of(
            "",
            "",
            r#"
[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:14580"

[[listen]]
name = "Full feed"
kind = "fullfeed"
bind = "127.0.0.1:10152"
"#,
        );
        let report = compare(&base(), &added);
        assert_eq!(
            report.requires_restart.first().map(|c| c.setting),
            Some("listen")
        );
        assert_eq!(
            report.requires_restart.first().map(|c| c.to.as_str()),
            Some("2 listeners")
        );
    }

    /// A port's forced filter is captured into the listener context at bind time and
    /// handed to every connection task, so swapping the configuration does not reach it.
    #[test]
    fn changing_a_forced_filter_requires_a_restart() {
        let filtered = config_of(
            "",
            "",
            r#"
[[listen]]
name = "Clients"
kind = "igate"
bind = "127.0.0.1:14580"
filter = "m/350"
"#,
        );
        let report = compare(&base(), &filtered);
        assert_eq!(
            report.requires_restart.first().map(|c| c.setting),
            Some("listen.filter")
        );
    }

    /// A reload can carry both kinds at once, and must not let one hide the other.
    #[test]
    fn applicable_and_inapplicable_changes_are_reported_together() {
        let mut both = config_of(r#"admin = "Someone, N0CALL""#, "", "");
        both.server.id = "T2OTHER".to_owned();

        let report = compare(&base(), &both);
        assert_eq!(report.applied.len(), 1, "the contact detail is adopted");
        assert_eq!(report.requires_restart.len(), 1, "the callsign is not");
        assert!(report.needs_restart());
    }
}
