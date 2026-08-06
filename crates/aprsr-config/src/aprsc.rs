//! Importer for `aprsc.conf` files.
//!
//! aprsc uses a directive-per-line configuration format. Sysops migrating an existing
//! server should not have to retype their setup, so `aprsr convert-config` reads that
//! format and emits the equivalent `aprsr.toml`.
//!
//! Conversion is deliberately lossy in one direction only: directives aprsr does not yet
//! implement are reported as [`Warning`]s rather than silently dropped, so the operator
//! knows exactly what did not survive the move.

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::duration::Interval;
use crate::{
    Config, Database, Http, Limits, Listener, PortKind, Protocol, Server, Uplink, UplinkKind,
};

/// Why an `aprsc.conf` file could not be read at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConvertError {
    #[error("line {line}: unterminated quoted string")]
    UnterminatedQuote { line: usize },
    #[error("line {line}: {directive} needs {expected} arguments, got {found}")]
    WrongArgumentCount {
        line: usize,
        directive: String,
        expected: &'static str,
        found: usize,
    },
    #[error("line {line}: {value:?} is not a valid {what}")]
    InvalidValue {
        line: usize,
        what: &'static str,
        value: String,
    },
    #[error("line {line}: {address:?} is not a valid address and port")]
    InvalidAddress { line: usize, address: String },
}

/// Something that did not convert cleanly but does not stop the conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// A directive aprsr does not recognise at all.
    Unknown { line: usize, directive: String },
    /// A directive that is understood but has no aprsr equivalent yet.
    NotSupported {
        line: usize,
        directive: String,
        reason: &'static str,
    },
    /// A listener option that was dropped.
    OptionDropped {
        line: usize,
        option: String,
        reason: &'static str,
    },
    /// An unnamed listener that had a name generated for it.
    NameGenerated { line: usize, generated: String },
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { line, directive } => {
                write!(f, "line {line}: unknown directive {directive:?}, ignored")
            }
            Self::NotSupported {
                line,
                directive,
                reason,
            } => {
                write!(f, "line {line}: {directive} not converted — {reason}")
            }
            Self::OptionDropped {
                line,
                option,
                reason,
            } => {
                write!(
                    f,
                    "line {line}: listener option {option:?} dropped — {reason}"
                )
            }
            Self::NameGenerated { line, generated } => {
                write!(
                    f,
                    "line {line}: unnamed listener, generated the name {generated:?}"
                )
            }
        }
    }
}

/// The result of importing an `aprsc.conf`.
#[derive(Debug, Clone)]
pub struct Conversion {
    /// The equivalent aprsr configuration. Not validated — the source file may itself be
    /// incomplete, and the operator should see the conversion either way.
    pub config: Config,
    /// Everything that did not convert cleanly.
    pub warnings: Vec<Warning>,
}

/// Convert an `aprsc.conf` into an aprsr [`Config`].
pub fn convert(text: &str) -> Result<Conversion, ConvertError> {
    let mut server = Server {
        id: crate::PLACEHOLDER_SERVER_ID.to_owned(),
        passcode: 0,
        admin: String::new(),
        email: String::new(),
        run_dir: PathBuf::from("data"),
    };
    let mut limits = Limits::default();
    let mut http = Http::default();
    let mut listeners: Vec<Listener> = Vec::new();
    let mut uplinks: Vec<Uplink> = Vec::new();
    let mut warnings: Vec<Warning> = Vec::new();

    for (index, raw_line) in text.lines().enumerate() {
        let line = index + 1;
        let tokens = tokenize(raw_line, line)?;
        let Some(directive) = tokens.first() else {
            continue;
        };
        let args = tokens.get(1..).unwrap_or(&[]);

        match directive.to_ascii_lowercase().as_str() {
            "serverid" => server.id = single(line, directive, args)?,
            "passcode" => {
                server.passcode = number(line, "passcode", &single(line, directive, args)?)?;
            }
            "myadmin" => server.admin = single(line, directive, args)?,
            "myemail" => server.email = single(line, directive, args)?,
            "rundir" => server.run_dir = PathBuf::from(single(line, directive, args)?),
            "filelimit" => {
                limits.file_limit = number(line, "file limit", &single(line, directive, args)?)?;
            }
            "clienttimeout" => {
                limits.client_timeout = interval(line, &single(line, directive, args)?)?;
            }
            "upstreamtimeout" => {
                limits.upstream_timeout = interval(line, &single(line, directive, args)?)?;
            }
            "listen" => {
                listeners.push(parse_listen(line, args, &mut warnings)?);
            }
            "uplink" => uplinks.push(parse_uplink(line, args)?),
            "httpstatus" => {
                http.status_bind = Some(address(line, args.first(), args.get(1))?);
            }
            "httpupload" => warnings.push(Warning::NotSupported {
                line,
                directive: directive.clone(),
                reason: "HTTP position upload is on the roadmap, not in this release",
            }),
            "uplinkbind" => warnings.push(Warning::NotSupported {
                line,
                directive: directive.clone(),
                reason: "uplinks are on the roadmap, not in this release",
            }),
            "logrotate" => warnings.push(Warning::NotSupported {
                line,
                directive: directive.clone(),
                reason: "aprsr logs to stdout; use your service manager's log rotation",
            }),
            "magicbadness" => warnings.push(Warning::NotSupported {
                line,
                directive: directive.clone(),
                reason: "this is aprsc's deliberate start-up tripwire and has no aprsr equivalent",
            }),
            _ => warnings.push(Warning::Unknown {
                line,
                directive: directive.clone(),
            }),
        }
    }

    Ok(Conversion {
        config: Config {
            server,
            limits,
            database: Database::default(),
            http,
            listeners,
            uplinks,
        },
        warnings,
    })
}

/// `Listen <name> <porttype> <proto> <address> <port> [options…]`
fn parse_listen(
    line: usize,
    args: &[String],
    warnings: &mut Vec<Warning>,
) -> Result<Listener, ConvertError> {
    if args.len() < 5 {
        return Err(ConvertError::WrongArgumentCount {
            line,
            directive: "Listen".to_owned(),
            expected: "at least 5",
            found: args.len(),
        });
    }

    let name = args.first().map_or("", String::as_str);
    let kind = match args.get(1).map_or("", String::as_str) {
        "fullfeed" => PortKind::FullFeed,
        "igate" => PortKind::Igate,
        "dupefeed" => PortKind::DupeFeed,
        "udpsubmit" => PortKind::UdpSubmit,
        other => {
            return Err(ConvertError::InvalidValue {
                line,
                what: "port type",
                value: other.to_owned(),
            });
        }
    };
    let protocol = match args.get(2).map_or("", String::as_str) {
        "tcp" => Protocol::Tcp,
        "udp" => Protocol::Udp,
        other => {
            return Err(ConvertError::InvalidValue {
                line,
                what: "protocol",
                value: other.to_owned(),
            });
        }
    };
    let bind = address(line, args.get(3), args.get(4))?;

    // aprsc pairs a TCP and a UDP listener on the same port by giving the second an empty
    // name. aprsr shows the name in logs and statistics and needs it to be unique.
    let name = if name.is_empty() {
        let generated = format!(
            "{} {} {}",
            args.get(1).map_or("", String::as_str),
            args.get(2).map_or("", String::as_str),
            bind.port()
        );
        warnings.push(Warning::NameGenerated {
            line,
            generated: generated.clone(),
        });
        generated
    } else {
        name.to_owned()
    };

    let mut listener = Listener {
        name,
        kind,
        protocol,
        bind,
        filter: None,
        max_clients: None,
        hidden: false,
    };

    let mut i = 5;
    while let Some(option) = args.get(i) {
        match option.to_ascii_lowercase().as_str() {
            "hidden" => {
                listener.hidden = true;
                i += 1;
            }
            "filter" => {
                listener.filter = args.get(i + 1).cloned();
                i += 2;
            }
            "maxclients" => {
                let value = args.get(i + 1).map_or("", String::as_str);
                listener.max_clients = Some(number(line, "client limit", value)?);
                i += 2;
            }
            "acl" => {
                warnings.push(Warning::OptionDropped {
                    line,
                    option: option.clone(),
                    reason: "address-based access control is on the roadmap",
                });
                i += 2;
            }
            _ => {
                warnings.push(Warning::OptionDropped {
                    line,
                    option: option.clone(),
                    reason: "unrecognised listener option",
                });
                i += 1;
            }
        }
    }

    Ok(listener)
}

/// `Uplink <name> <type> <proto> <host> <port>`
fn parse_uplink(line: usize, args: &[String]) -> Result<Uplink, ConvertError> {
    if args.len() < 5 {
        return Err(ConvertError::WrongArgumentCount {
            line,
            directive: "Uplink".to_owned(),
            expected: "5",
            found: args.len(),
        });
    }

    let kind = match args.get(1).map_or("", String::as_str) {
        "full" => UplinkKind::Full,
        "ro" => UplinkKind::ReadOnly,
        other => {
            return Err(ConvertError::InvalidValue {
                line,
                what: "uplink type",
                value: other.to_owned(),
            });
        }
    };

    // Uplink hosts are names, not addresses: `rotate.aprs.net` is a DNS rotation and must
    // be resolved fresh on every reconnection.
    let host = args.get(3).map_or("", String::as_str);
    let port = args.get(4).map_or("", String::as_str);
    let port: u16 = port.parse().map_err(|_| ConvertError::InvalidValue {
        line,
        what: "port",
        value: port.to_owned(),
    })?;

    Ok(Uplink {
        name: args.first().cloned().unwrap_or_default(),
        kind,
        address: format!("{host}:{port}"),
    })
}

/// Combine an aprsc address and port into a [`SocketAddr`].
fn address(
    line: usize,
    host: Option<&String>,
    port: Option<&String>,
) -> Result<SocketAddr, ConvertError> {
    let host = host.map_or("", String::as_str);
    let port = port.map_or("", String::as_str);
    // IPv6 literals must be bracketed before they can be parsed with a port.
    let combined = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    combined.parse().map_err(|_| ConvertError::InvalidAddress {
        line,
        address: combined,
    })
}

fn single(line: usize, directive: &str, args: &[String]) -> Result<String, ConvertError> {
    match args {
        [only] => Ok(only.clone()),
        _ => Err(ConvertError::WrongArgumentCount {
            line,
            directive: directive.to_owned(),
            expected: "1",
            found: args.len(),
        }),
    }
}

fn number<T: std::str::FromStr>(
    line: usize,
    what: &'static str,
    value: &str,
) -> Result<T, ConvertError> {
    value.parse().map_err(|_| ConvertError::InvalidValue {
        line,
        what,
        value: value.to_owned(),
    })
}

fn interval(line: usize, value: &str) -> Result<Interval, ConvertError> {
    Interval::parse(value).map_err(|_| ConvertError::InvalidValue {
        line,
        what: "interval",
        value: value.to_owned(),
    })
}

/// Split one configuration line into tokens.
///
/// Double quotes group a token containing spaces; `#` outside quotes starts a comment.
fn tokenize(raw: &str, line: usize) -> Result<Vec<String>, ConvertError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut in_quotes = false;

    for ch in raw.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                // An empty quoted string is a real token, which is how aprsc marks the
                // UDP twin of a TCP listener.
                has_token = true;
            }
            '#' if !in_quotes => break,
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    tokens.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }

    if in_quotes {
        return Err(ConvertError::UnterminatedQuote { line });
    }
    if has_token {
        tokens.push(current);
    }

    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn tokenizes_a_plain_directive() {
        assert_eq!(
            tokenize("ServerId N0CALL", 1).unwrap(),
            ["ServerId", "N0CALL"]
        );
    }

    #[test]
    fn tokenizes_quoted_strings_containing_spaces() {
        assert_eq!(
            tokenize(r#"MyAdmin "My Name, MYCALL""#, 1).unwrap(),
            ["MyAdmin", "My Name, MYCALL"]
        );
    }

    #[test]
    fn keeps_empty_quoted_strings_as_tokens() {
        // aprsc names the UDP twin of a TCP listener with an empty string.
        assert_eq!(
            tokenize(r#"Listen "" fullfeed udp :: 10152"#, 1).unwrap(),
            ["Listen", "", "fullfeed", "udp", "::", "10152"]
        );
    }

    #[rstest]
    #[case("# a whole-line comment")]
    #[case("   ")]
    #[case("")]
    fn ignores_comments_and_blank_lines(#[case] input: &str) {
        assert!(tokenize(input, 1).unwrap().is_empty());
    }

    #[test]
    fn strips_trailing_comments() {
        assert_eq!(
            tokenize("FileLimit 10000 # as many as we can", 1).unwrap(),
            ["FileLimit", "10000"]
        );
    }

    #[test]
    fn a_hash_inside_quotes_is_not_a_comment() {
        assert_eq!(
            tokenize(r#"MyAdmin "Name #1""#, 1).unwrap(),
            ["MyAdmin", "Name #1"]
        );
    }

    #[test]
    fn rejects_an_unterminated_quote() {
        assert_eq!(
            tokenize(r#"MyAdmin "unclosed"#, 7).unwrap_err(),
            ConvertError::UnterminatedQuote { line: 7 }
        );
    }

    #[test]
    fn converts_server_identity() {
        let converted = convert(
            r#"
ServerId   OH7LZB-1
PassCode   13023
MyAdmin    "Someone, N0CALL"
MyEmail    someone@example.com
RunDir     /var/lib/aprsr
"#,
        )
        .expect("converts");

        assert_eq!(converted.config.server.id, "OH7LZB-1");
        assert_eq!(converted.config.server.passcode, 13023);
        assert_eq!(converted.config.server.admin, "Someone, N0CALL");
        assert_eq!(converted.config.server.email, "someone@example.com");
        assert_eq!(
            converted.config.server.run_dir,
            PathBuf::from("/var/lib/aprsr")
        );
    }

    #[test]
    fn converts_intervals_and_limits() {
        let converted =
            convert("UpstreamTimeout 15s\nClientTimeout 48h\nFileLimit 20000\n").expect("converts");
        assert_eq!(converted.config.limits.upstream_timeout.as_secs(), 15);
        assert_eq!(converted.config.limits.client_timeout.as_secs(), 172_800);
        assert_eq!(converted.config.limits.file_limit, 20_000);
    }

    #[test]
    fn converts_a_listener_with_options() {
        let converted = convert(
            r#"Listen "350 km from my position" igate tcp :: 20350 filter "m/350" maxclients 100 hidden"#,
        )
        .expect("converts");

        let listener = converted.config.listeners.first().expect("one listener");
        assert_eq!(listener.name, "350 km from my position");
        assert_eq!(listener.kind, PortKind::Igate);
        assert_eq!(listener.protocol, Protocol::Tcp);
        assert_eq!(listener.bind.to_string(), "[::]:20350");
        assert_eq!(listener.filter.as_deref(), Some("m/350"));
        assert_eq!(listener.max_clients, Some(100));
        assert!(listener.hidden);
    }

    #[test]
    fn converts_an_ipv4_listener() {
        let converted = convert("Listen \"Clients\" igate tcp 0.0.0.0 14580").expect("converts");
        let listener = converted.config.listeners.first().expect("one listener");
        assert_eq!(listener.bind.to_string(), "0.0.0.0:14580");
    }

    #[test]
    fn generates_a_name_for_an_unnamed_listener() {
        let converted = convert("Listen \"\" fullfeed udp :: 10152 hidden").expect("converts");
        let listener = converted.config.listeners.first().expect("one listener");
        assert_eq!(listener.name, "fullfeed udp 10152");
        assert!(matches!(
            converted.warnings.first(),
            Some(Warning::NameGenerated { .. })
        ));
    }

    #[test]
    fn converts_uplinks() {
        let converted = convert(
            "Uplink \"Core rotate\" full tcp rotate.aprs.net 10152\n\
             Uplink \"Read only\" ro tcp second.example.net 10152\n",
        )
        .expect("converts");

        assert_eq!(converted.config.uplinks.len(), 2);
        let first = converted.config.uplinks.first().expect("first uplink");
        assert_eq!(first.name, "Core rotate");
        assert_eq!(first.kind, UplinkKind::Full);
        assert_eq!(first.address, "rotate.aprs.net:10152");
        assert_eq!(
            converted.config.uplinks.get(1).map(|u| u.kind),
            Some(UplinkKind::ReadOnly)
        );
    }

    #[test]
    fn converts_the_http_status_port() {
        let converted = convert("HTTPStatus 0.0.0.0 14501").expect("converts");
        assert_eq!(
            converted.config.http.status_bind.map(|a| a.to_string()),
            Some("0.0.0.0:14501".to_owned())
        );
    }

    #[rstest]
    #[case("HTTPUpload 0.0.0.0 8080")]
    #[case("UplinkBind 127.0.0.1")]
    #[case("LogRotate 10 5")]
    #[case("MagicBadness 42.7")]
    fn reports_directives_with_no_equivalent(#[case] input: &str) {
        let converted = convert(input).expect("converts");
        assert!(
            matches!(
                converted.warnings.first(),
                Some(Warning::NotSupported { .. })
            ),
            "expected a NotSupported warning for {input}, got {:?}",
            converted.warnings
        );
    }

    #[test]
    fn reports_unknown_directives() {
        let converted = convert("SomethingInvented 42").expect("converts");
        assert_eq!(
            converted.warnings,
            [Warning::Unknown {
                line: 1,
                directive: "SomethingInvented".to_owned()
            }]
        );
    }

    #[test]
    fn reports_a_dropped_acl_option() {
        let converted =
            convert("Listen \"Clients\" igate tcp :: 14580 acl etc/client.acl").expect("converts");
        assert!(matches!(
            converted.warnings.first(),
            Some(Warning::OptionDropped { .. })
        ));
    }

    #[rstest]
    #[case("Listen \"Clients\" igate tcp ::", "at least 5")]
    #[case("Uplink \"Core\" full tcp host", "5")]
    fn rejects_directives_with_too_few_arguments(#[case] input: &str, #[case] expected: &str) {
        let err = convert(input).unwrap_err();
        assert!(
            matches!(&err, ConvertError::WrongArgumentCount { expected: e, .. } if *e == expected),
            "got {err:?}"
        );
    }

    #[test]
    fn rejects_an_unknown_port_type() {
        assert_eq!(
            convert("Listen \"Clients\" nonsense tcp :: 14580").unwrap_err(),
            ConvertError::InvalidValue {
                line: 1,
                what: "port type",
                value: "nonsense".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_an_unparseable_address() {
        assert!(matches!(
            convert("HTTPStatus not-an-address 14501").unwrap_err(),
            ConvertError::InvalidAddress { .. }
        ));
    }

    #[test]
    fn line_numbers_in_errors_point_at_the_real_line() {
        let err = convert("# comment\n\nServerId N0CALL\nListen \"x\" bogus tcp :: 1").unwrap_err();
        assert!(
            matches!(err, ConvertError::InvalidValue { line: 4, .. }),
            "got {err:?}"
        );
    }
}
