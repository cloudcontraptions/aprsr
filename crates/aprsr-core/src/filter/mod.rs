//! APRS-IS server-side filters.
//!
//! A client on an `igate` port describes the slice of the feed it wants with a filter
//! expression: a space-separated list of filters, each `letter/argument/argument…`.
//! Filters are **additive** — a packet is delivered if any of them matches — and any
//! filter may be prefixed with `-` to subtract instead.
//!
//! Syntax and semantics come from <http://www.aprs-is.net/javAPRSFilter.aspx>.
//!
//! ```
//! use aprsr_core::filter::{FilterChain, MatchContext, NoPositions};
//! use aprsr_core::{aprs, Tnc2Packet};
//!
//! let chain = FilterChain::parse("r/60.2/24.9/50 -b/N0SPAM").unwrap();
//! let packet = Tnc2Packet::parse("N0CALL>APRS:=6012.30N/02456.78E-").unwrap();
//! let parsed = aprs::parse(&packet);
//! assert!(chain.matches(&MatchContext {
//!     packet: &packet,
//!     parsed: &parsed,
//!     client: None,
//!     positions: &NoPositions,
//! }));
//! ```

use crate::aprs::{PacketType, ParsedPayload, Position};
use crate::callsign::matches_pattern;
use crate::geo::{great_circle_distance_km, within_area};
use crate::packet::Tnc2Packet;
use crate::qconstruct;

/// Most filters permitted in one expression.
///
/// Filter expressions arrive from unauthenticated clients, so the chain length is bounded
/// to stop a single login from allocating without limit.
pub const MAX_FILTERS: usize = 64;

/// Most callsigns permitted in one list-valued filter.
pub const MAX_LIST_ENTRIES: usize = 64;

/// Most `a/` filters permitted, per the specification.
pub const MAX_AREA_FILTERS: usize = 9;

/// Where the last known position of a station comes from.
///
/// The `m/` and `f/` filters are relative to a station's most recent position, which lives
/// in the database rather than in the packet being matched. Keeping this a trait is what
/// lets `aprsr-core` stay free of any storage dependency.
pub trait PositionSource: std::fmt::Debug {
    /// The last known position of `callsign`, if one is known.
    fn position_of(&self, callsign: &str) -> Option<Position>;
}

/// A [`PositionSource`] that knows nothing. `m/` and `f/` filters never match against it.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPositions;

impl PositionSource for NoPositions {
    fn position_of(&self, _callsign: &str) -> Option<Position> {
        None
    }
}

/// Everything a filter needs in order to decide about one packet.
#[derive(Debug, Clone, Copy)]
pub struct MatchContext<'a> {
    /// The packet being considered.
    pub packet: &'a Tnc2Packet<'a>,
    /// Its parsed information field.
    pub parsed: &'a ParsedPayload<'a>,
    /// The callsign of the client this filter chain belongs to, for `m/`.
    pub client: Option<&'a str>,
    /// Station position lookup, for `m/` and `f/`.
    pub positions: &'a dyn PositionSource,
}

/// Why a filter expression could not be parsed.
///
/// Not `Eq`, because the coordinate variants carry the offending `f64` back to the client.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FilterError {
    #[error("filter expression is empty")]
    Empty,
    #[error("expression has more than {MAX_FILTERS} filters")]
    TooManyFilters,
    #[error("expression has more than {MAX_AREA_FILTERS} area filters")]
    TooManyAreaFilters,
    #[error("filter {list:?} has more than {MAX_LIST_ENTRIES} entries")]
    TooManyEntries { list: String },
    #[error("unknown filter type {code:?}")]
    UnknownType { code: String },
    #[error("filter {code}/ needs {expected} arguments, got {found}")]
    WrongArgumentCount {
        code: String,
        expected: &'static str,
        found: usize,
    },
    #[error("filter {code}/ has an empty argument list")]
    EmptyArgumentList { code: String },
    #[error("{what} {value:?} is not a number")]
    NotANumber { what: &'static str, value: String },
    #[error("latitude {value} is outside -90..90")]
    LatitudeOutOfRange { value: f64 },
    #[error("longitude {value} is outside -180..180")]
    LongitudeOutOfRange { value: f64 },
    #[error("distance {value} must be greater than zero")]
    DistanceOutOfRange { value: f64 },
    #[error("type filter {value:?} contains a letter that is not one of poimqstunw")]
    UnknownTypeLetter { value: String },
    #[error("an os/ filter must be the last filter on the line")]
    StrictObjectNotLast,
    #[error("an expression may contain only one os/ filter")]
    MultipleStrictObject,
}

/// A single parsed filter.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    /// `r/lat/lon/dist` — positions within `dist` km of a point.
    Range {
        latitude: f64,
        longitude: f64,
        distance_km: f64,
    },
    /// `p/aa/bb/…` — source callsigns starting with any of these prefixes.
    Prefix(Box<[Box<str>]>),
    /// `b/call1/call2/…` — traffic from these exact callsigns.
    Budlist(Box<[Box<str>]>),
    /// `o/obj1/obj2/…` — objects and items with these names.
    Object(Box<[Box<str>]>),
    /// `os/` — the strict object filter.
    ///
    /// Per <http://www.aprs-is.net/javAPRSFilter.aspx>: "Pass all objects with the exact
    /// name of obj1, obj2, ... Objects are always 9 characters and Items are 3 to 9
    /// characters. There can only be one os filter and that filter must be at the end of
    /// the line."
    ///
    /// The two constraints in that last sentence are not arbitrary, and they are the whole
    /// point of the filter. Unlike `o/`, whose specification says "spaces not allowed",
    /// `os/` carries no such restriction — and object names are a fixed nine-character
    /// field that may well contain spaces. Since a filter expression is split on
    /// whitespace, a name with a space in it can only be written by taking the rest of the
    /// line, which is possible for exactly one filter and only if it comes last.
    ///
    /// Matching itself is identical to [`Filter::Object`]. The sentence about lengths
    /// describes the *packet format*, not an extra test applied here: `aprs::parse` already
    /// reads objects from a nine-character field and items from a three-to-nine-character
    /// one, and trims the padding. Re-checking the trimmed name against those lengths would
    /// reject `FIELDDAY`, which is a perfectly ordinary object.
    StrictObject(Box<[Box<str>]>),
    /// `t/poimqstunw` or `t/poimqstunw/call/km` — packet categories, optionally limited
    /// to a radius around a station.
    Type {
        types: PacketType,
        near: Option<(Box<str>, f64)>,
    },
    /// `s/pri/alt/over` — symbol selection.
    Symbol {
        primary: Box<str>,
        alternate: Box<str>,
        overlay: Box<str>,
    },
    /// `d/digi1/digi2/…` — packets digipeated by these stations.
    Digipeater(Box<[Box<str>]>),
    /// `a/latN/lonW/latS/lonE` — positions inside a box.
    Area {
        lat_north: f64,
        lon_west: f64,
        lat_south: f64,
        lon_east: f64,
    },
    /// `e/call1/call2/…` — packets that entered APRS-IS through these stations.
    Entry(Box<[Box<str>]>),
    /// `g/call1/call2/…` — messages addressed to these callsigns.
    Group(Box<[Box<str>]>),
    /// `u/dest1/dest2/…` — packets with these destination (unproto) addresses.
    Unproto(Box<[Box<str>]>),
    /// `q/con/ana` — q construct selection.
    QConstruct {
        constructs: Box<str>,
        igate_positions: bool,
    },
    /// `m/dist` — within `dist` km of the client's own last known position.
    MyRange { distance_km: f64 },
    /// `f/call/dist` — within `dist` km of another station's last known position.
    FriendRange {
        callsign: Box<str>,
        distance_km: f64,
    },
}

impl Filter {
    /// Parse one filter, without the `-` negation prefix.
    pub fn parse(text: &str) -> Result<Self, FilterError> {
        let mut parts = text.split('/');
        let code = parts.next().unwrap_or("");
        let args: Vec<&str> = parts.collect();

        match code {
            "r" => {
                let [lat, lon, dist] = exact(code, &args, "3")?;
                Ok(Self::Range {
                    latitude: latitude(lat)?,
                    longitude: longitude(lon)?,
                    distance_km: distance(dist)?,
                })
            }
            "p" => Ok(Self::Prefix(list(code, &args)?)),
            "b" => Ok(Self::Budlist(list(code, &args)?)),
            "o" => Ok(Self::Object(list(code, &args)?)),
            // Reached only through `FilterChain::parse`, which hands over the whole
            // remainder of the line. Parsing `os/` on its own here would silently drop any
            // name containing a space, which is the one thing this filter exists to allow.
            "os" => Ok(Self::StrictObject(list(code, &args)?)),
            "d" => Ok(Self::Digipeater(list(code, &args)?)),
            "e" => Ok(Self::Entry(list(code, &args)?)),
            "g" => Ok(Self::Group(list(code, &args)?)),
            "u" => Ok(Self::Unproto(list(code, &args)?)),
            "t" => parse_type(&args),
            "s" => {
                // s/pri, s/pri/alt and s/pri/alt/over are all legal.
                if args.is_empty() || args.len() > 3 {
                    return Err(FilterError::WrongArgumentCount {
                        code: code.to_owned(),
                        expected: "1 to 3",
                        found: args.len(),
                    });
                }
                Ok(Self::Symbol {
                    primary: args.first().copied().unwrap_or("").into(),
                    alternate: args.get(1).copied().unwrap_or("").into(),
                    overlay: args.get(2).copied().unwrap_or("").into(),
                })
            }
            "a" => {
                let [n, w, s, e] = exact(code, &args, "4")?;
                Ok(Self::Area {
                    lat_north: latitude(n)?,
                    lon_west: longitude(w)?,
                    lat_south: latitude(s)?,
                    lon_east: longitude(e)?,
                })
            }
            "q" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(FilterError::WrongArgumentCount {
                        code: code.to_owned(),
                        expected: "1 or 2",
                        found: args.len(),
                    });
                }
                Ok(Self::QConstruct {
                    constructs: args.first().copied().unwrap_or("").into(),
                    igate_positions: args.get(1).is_some_and(|a| a.contains('I')),
                })
            }
            "m" => {
                let [dist] = exact(code, &args, "1")?;
                Ok(Self::MyRange {
                    distance_km: distance(dist)?,
                })
            }
            "f" => {
                let [call, dist] = exact(code, &args, "2")?;
                Ok(Self::FriendRange {
                    callsign: call.to_ascii_uppercase().into(),
                    distance_km: distance(dist)?,
                })
            }
            other => Err(FilterError::UnknownType {
                code: other.to_owned(),
            }),
        }
    }

    /// Decide whether one packet matches this filter.
    #[must_use]
    pub fn matches(&self, ctx: &MatchContext<'_>) -> bool {
        match self {
            Self::Range {
                latitude,
                longitude,
                distance_km,
            } => within(ctx.parsed.position, *latitude, *longitude, *distance_km),
            Self::Prefix(prefixes) => prefixes.iter().any(|p| {
                let source = ctx.packet.source();
                source.len() >= p.len()
                    && source
                        .get(..p.len())
                        .is_some_and(|head| head.eq_ignore_ascii_case(p))
            }),
            Self::Budlist(calls) => calls
                .iter()
                .any(|c| matches_pattern(c, ctx.packet.source())),
            Self::Object(names) => ctx
                .parsed
                .object_name
                .is_some_and(|name| names.iter().any(|n| matches_pattern(n, name))),
            // Matching is the same as `o/`. The difference is entirely in what can be
            // *asked for*: because this filter takes the rest of the line, its argument may
            // contain spaces, and so it can name an object that `o/` cannot address at all.
            Self::StrictObject(names) => ctx
                .parsed
                .object_name
                .is_some_and(|name| names.iter().any(|n| matches_pattern(n, name))),
            Self::Digipeater(calls) => ctx
                .packet
                .path()
                .hops()
                // "digipeated by" means the station actually repeated the packet, which is
                // what the used flag records.
                .filter(|hop| hop.used)
                .any(|hop| calls.iter().any(|c| matches_pattern(c, hop.call))),
            Self::Entry(calls) => entry_station(ctx.packet)
                .is_some_and(|entry| calls.iter().any(|c| matches_pattern(c, entry))),
            Self::Group(calls) => ctx
                .parsed
                .addressee
                .is_some_and(|to| calls.iter().any(|c| matches_pattern(c, to))),
            Self::Unproto(dests) => dests
                .iter()
                .any(|d| matches_pattern(d, ctx.packet.destination())),
            Self::Type { types, near } => {
                ctx.parsed.types.intersects(*types)
                    && match near {
                        None => true,
                        Some((call, km)) => near_station(ctx, call, *km),
                    }
            }
            Self::Symbol {
                primary,
                alternate,
                overlay,
            } => matches_symbol(ctx, primary, alternate, overlay),
            Self::Area {
                lat_north,
                lon_west,
                lat_south,
                lon_east,
            } => ctx.parsed.position.is_some_and(|p| {
                within_area(
                    p.latitude,
                    p.longitude,
                    *lat_north,
                    *lon_west,
                    *lat_south,
                    *lon_east,
                )
            }),
            Self::QConstruct {
                constructs,
                igate_positions,
            } => matches_qconstruct(ctx, constructs, *igate_positions),
            Self::MyRange { distance_km } => ctx
                .client
                .is_some_and(|call| near_station(ctx, call, *distance_km)),
            Self::FriendRange {
                callsign,
                distance_km,
            } => near_station(ctx, callsign, *distance_km),
        }
    }
}

/// True when the packet has a position within `km` of `callsign`'s last known position.
///
/// Backs `m/`, `f/` and the optional radius on `t/`. A station whose position is unknown
/// can never be a centre, so the filter matches nothing rather than matching everything.
fn near_station(ctx: &MatchContext<'_>, callsign: &str, km: f64) -> bool {
    ctx.positions
        .position_of(callsign)
        .is_some_and(|centre| within(ctx.parsed.position, centre.latitude, centre.longitude, km))
}

/// `s/pri/alt/over` matching.
fn matches_symbol(ctx: &MatchContext<'_>, primary: &str, alternate: &str, overlay: &str) -> bool {
    let Some(symbol) = ctx.parsed.symbol else {
        return false;
    };
    match symbol.table {
        '/' => primary.contains(symbol.code),
        '\\' => alternate.contains(symbol.code),
        // An overlay character selects the alternate table with a character drawn over
        // it; both the overlay and the symbol code must be wanted.
        table => {
            overlay.contains(table) && (alternate.is_empty() || alternate.contains(symbol.code))
        }
    }
}

/// `q/con/ana` matching.
fn matches_qconstruct(ctx: &MatchContext<'_>, constructs: &str, igate_positions: bool) -> bool {
    let Some(found) = qconstruct::find(ctx.packet.path()) else {
        return false;
    };
    // The filter names constructs by their distinguishing third character: `q/CS` passes
    // qAC and qAS. Matching is case-sensitive because qAo and qAO differ only in case and
    // mean different things.
    if found
        .code
        .as_str()
        .chars()
        .nth(2)
        .is_some_and(|c| constructs.contains(c))
    {
        return true;
    }

    // The `I` analysis flag additionally passes position packets that entered through an
    // IGate, whatever construct letter the client asked for.
    igate_positions
        && ctx.parsed.types.intersects(PacketType::POSITION)
        && matches!(
            found.code,
            qconstruct::QCode::VerifiedIgate | qconstruct::QCode::RemoteIgate
        )
}

/// One entry in a chain: a filter and whether it subtracts rather than adds.
#[derive(Debug, Clone, PartialEq)]
pub struct FilterEntry {
    pub negated: bool,
    pub filter: Filter,
}

/// A complete filter expression.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FilterChain {
    entries: Vec<FilterEntry>,
}

impl FilterChain {
    /// Parse a whitespace-separated filter expression.
    ///
    /// An empty expression yields an empty chain, which matches nothing — that is the
    /// documented behaviour of a filtered port: "clients begin receiving minimal packets,
    /// then accumulate messages matching defined criteria".
    pub fn parse(expression: &str) -> Result<Self, FilterError> {
        let mut entries = Vec::new();
        let mut area_filters = 0usize;

        // Byte offsets are tracked alongside the tokens so that an `os/` filter can take
        // the rest of the line verbatim, spaces and all.
        let mut remainder = expression;
        while let Some(token) = next_token(&mut remainder) {
            if entries.len() >= MAX_FILTERS {
                return Err(FilterError::TooManyFilters);
            }
            let (negated, body) = match token.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, token),
            };
            if body.is_empty() {
                return Err(FilterError::Empty);
            }

            // `os/` is the one filter whose argument may contain spaces, so it takes the
            // rest of the line. That is why the specification allows only one and requires
            // it last — but "requires" is worth enforcing rather than assuming, because
            // absorbing a filter the operator wrote after it would silently deliver a
            // different feed than they asked for.
            let filter = if is_strict_object(body) {
                if let Some(stray) = remainder
                    .split_ascii_whitespace()
                    .find(|token| looks_like_a_filter(token))
                {
                    // Almost certainly a filter the operator expected to take effect; it
                    // would otherwise have disappeared into an object name. The
                    // specification forbids both a second `os/` and anything after the
                    // first, so say which of the two happened.
                    let stray_body = stray.strip_prefix('-').unwrap_or(stray);
                    return Err(if is_strict_object(stray_body) {
                        FilterError::MultipleStrictObject
                    } else {
                        FilterError::StrictObjectNotLast
                    });
                }
                let whole = if remainder.is_empty() {
                    body.to_owned()
                } else {
                    format!("{body} {remainder}")
                };
                remainder = "";
                Filter::parse(&whole)?
            } else {
                Filter::parse(body)?
            };
            if matches!(filter, Filter::Area { .. }) {
                area_filters += 1;
                if area_filters > MAX_AREA_FILTERS {
                    return Err(FilterError::TooManyAreaFilters);
                }
            }
            entries.push(FilterEntry { negated, filter });
        }

        Ok(Self { entries })
    }

    /// The parsed entries.
    #[must_use]
    pub fn entries(&self) -> &[FilterEntry] {
        &self.entries
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Decide whether a packet should be delivered to this client.
    ///
    /// Negation wins: if any `-` filter matches, the packet is dropped regardless of what
    /// else matched. Otherwise the packet passes when at least one positive filter matches.
    #[must_use]
    pub fn matches(&self, ctx: &MatchContext<'_>) -> bool {
        let mut passed = false;
        for entry in &self.entries {
            if entry.filter.matches(ctx) {
                if entry.negated {
                    return false;
                }
                passed = true;
            }
        }
        passed
    }
}

impl std::fmt::Display for Filter {
    /// Render back to the wire syntax, so the status page and logs can echo a client's
    /// filter without keeping the original string around.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn joined(
            f: &mut std::fmt::Formatter<'_>,
            code: &str,
            items: &[Box<str>],
        ) -> std::fmt::Result {
            f.write_str(code)?;
            for item in items {
                write!(f, "/{item}")?;
            }
            Ok(())
        }

        match self {
            Self::Range {
                latitude,
                longitude,
                distance_km,
            } => {
                write!(f, "r/{latitude}/{longitude}/{distance_km}")
            }
            Self::Prefix(items) => joined(f, "p", items),
            Self::Budlist(items) => joined(f, "b", items),
            Self::Object(items) => joined(f, "o", items),
            Self::StrictObject(items) => joined(f, "os", items),
            Self::Digipeater(items) => joined(f, "d", items),
            Self::Entry(items) => joined(f, "e", items),
            Self::Group(items) => joined(f, "g", items),
            Self::Unproto(items) => joined(f, "u", items),
            Self::Type { types, near } => {
                f.write_str("t/")?;
                for letter in b"poimqstunw" {
                    if PacketType::from_filter_letter(*letter).is_some_and(|t| types.contains(t)) {
                        write!(f, "{}", *letter as char)?;
                    }
                }
                if let Some((call, km)) = near {
                    write!(f, "/{call}/{km}")?;
                }
                Ok(())
            }
            Self::Symbol {
                primary,
                alternate,
                overlay,
            } => {
                if overlay.is_empty() && alternate.is_empty() {
                    write!(f, "s/{primary}")
                } else if overlay.is_empty() {
                    write!(f, "s/{primary}/{alternate}")
                } else {
                    write!(f, "s/{primary}/{alternate}/{overlay}")
                }
            }
            Self::Area {
                lat_north,
                lon_west,
                lat_south,
                lon_east,
            } => {
                write!(f, "a/{lat_north}/{lon_west}/{lat_south}/{lon_east}")
            }
            Self::QConstruct {
                constructs,
                igate_positions,
            } => {
                if *igate_positions {
                    write!(f, "q/{constructs}/I")
                } else {
                    write!(f, "q/{constructs}")
                }
            }
            Self::MyRange { distance_km } => write!(f, "m/{distance_km}"),
            Self::FriendRange {
                callsign,
                distance_km,
            } => {
                write!(f, "f/{callsign}/{distance_km}")
            }
        }
    }
}

impl std::fmt::Display for FilterChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, entry) in self.entries.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            if entry.negated {
                f.write_str("-")?;
            }
            write!(f, "{}", entry.filter)?;
        }
        Ok(())
    }
}

// --- parsing helpers ---------------------------------------------------------------

fn exact<'a, const N: usize>(
    code: &str,
    args: &[&'a str],
    expected: &'static str,
) -> Result<[&'a str; N], FilterError> {
    <[&'a str; N]>::try_from(args).map_err(|_| FilterError::WrongArgumentCount {
        code: code.to_owned(),
        expected,
        found: args.len(),
    })
}

/// Take the next whitespace-separated token, leaving the rest of the line in `remainder`.
///
/// Returned separately from `split_ascii_whitespace` so that `os/` can claim everything
/// still unconsumed, including the spaces inside an object name.
fn next_token<'a>(remainder: &mut &'a str) -> Option<&'a str> {
    let trimmed = remainder.trim_start();
    if trimmed.is_empty() {
        *remainder = "";
        return None;
    }
    let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    let (token, rest) = trimmed.split_at(end);
    *remainder = rest.trim_start();
    Some(token)
}

/// Whether a token is the strict object filter, negated or not.
fn is_strict_object(body: &str) -> bool {
    let code = body.split('/').next().unwrap_or_default();
    code.eq_ignore_ascii_case("os")
}

/// Whether a token looks like somebody meant it as a filter rather than as an object name.
///
/// Used only to catch a filter written *after* `os/`, which would otherwise be swallowed
/// into an object name. One or two letters and a slash is the shape every filter code has.
fn looks_like_a_filter(token: &str) -> bool {
    let body = token.strip_prefix('-').unwrap_or(token);
    let Some((code, _)) = body.split_once('/') else {
        return false;
    };
    !code.is_empty() && code.len() <= 2 && code.chars().all(|c| c.is_ascii_alphabetic())
}

fn list(code: &str, args: &[&str]) -> Result<Box<[Box<str>]>, FilterError> {
    if args.is_empty() || args.iter().all(|a| a.is_empty()) {
        return Err(FilterError::EmptyArgumentList {
            code: code.to_owned(),
        });
    }
    if args.len() > MAX_LIST_ENTRIES {
        return Err(FilterError::TooManyEntries {
            list: code.to_owned(),
        });
    }
    Ok(args
        .iter()
        .filter(|a| !a.is_empty())
        .map(|a| a.to_ascii_uppercase().into_boxed_str())
        .collect())
}

fn parse_type(args: &[&str]) -> Result<Filter, FilterError> {
    let letters = args.first().copied().unwrap_or("");
    if letters.is_empty() {
        return Err(FilterError::EmptyArgumentList {
            code: "t".to_owned(),
        });
    }

    let mut types = PacketType::NONE;
    for letter in letters.bytes() {
        match PacketType::from_filter_letter(letter.to_ascii_lowercase()) {
            Some(t) => types |= t,
            None => {
                return Err(FilterError::UnknownTypeLetter {
                    value: letters.to_owned(),
                });
            }
        }
    }

    let near = match (args.get(1), args.get(2)) {
        (Some(call), Some(km)) => Some((call.to_ascii_uppercase().into_boxed_str(), distance(km)?)),
        (None, None) => None,
        _ => {
            return Err(FilterError::WrongArgumentCount {
                code: "t".to_owned(),
                expected: "1 or 3",
                found: args.len(),
            });
        }
    };

    Ok(Filter::Type { types, near })
}

fn number(what: &'static str, value: &str) -> Result<f64, FilterError> {
    value
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| FilterError::NotANumber {
            what,
            value: value.to_owned(),
        })
}

fn latitude(value: &str) -> Result<f64, FilterError> {
    let v = number("latitude", value)?;
    if (-90.0..=90.0).contains(&v) {
        Ok(v)
    } else {
        Err(FilterError::LatitudeOutOfRange { value: v })
    }
}

fn longitude(value: &str) -> Result<f64, FilterError> {
    let v = number("longitude", value)?;
    if (-180.0..=180.0).contains(&v) {
        Ok(v)
    } else {
        Err(FilterError::LongitudeOutOfRange { value: v })
    }
}

fn distance(value: &str) -> Result<f64, FilterError> {
    let v = number("distance", value)?;
    if v > 0.0 {
        Ok(v)
    } else {
        Err(FilterError::DistanceOutOfRange { value: v })
    }
}

/// Inclusive range test against an optional position.
fn within(position: Option<Position>, lat: f64, lon: f64, km: f64) -> bool {
    position.is_some_and(|p| great_circle_distance_km(lat, lon, p.latitude, p.longitude) <= km)
}

/// The callsign that follows the q construct — the station that injected the packet.
fn entry_station<'a>(packet: &Tnc2Packet<'a>) -> Option<&'a str> {
    qconstruct::find(packet.path()).and_then(|found| found.call)
}

#[cfg(test)]
mod tests;
