//! The dashboard's built assets, compiled into the binary.
//!
//! They used to be read from `CARGO_MANIFEST_DIR/static` at runtime, which works exactly
//! once: on the machine that compiled the binary, with the source tree still in place. Move
//! the binary anywhere else — `cargo install`, an `scp` to a server, a container built in one
//! stage and run in another — and every asset 404s while the HTML renders perfectly, so the
//! dashboard comes up unstyled with nothing in the log to say why.
//!
//! Embedding them removes the failure mode rather than documenting it. Total, at the time of
//! writing, about 250 KB — which is nothing next to being able to copy one file to a server
//! and have it work.
//!
//! ## Why the list is written out
//!
//! `include_bytes!` needs a literal path, so each asset is named here. That is a feature: a
//! new asset is one line, a *missing* one is a compile error rather than a silent 404, and
//! the list doubles as the record of what the npm build is expected to produce. CI already
//! checks the committed files match the sources; this checks they exist at all.

/// One embedded file.
#[derive(Debug, Clone, Copy)]
pub struct Asset {
    /// The name it is served as, under `/static/`.
    pub name: &'static str,
    pub content_type: &'static str,
    pub bytes: &'static [u8],
}

/// Everything the npm build produces, in the order the dashboard loads it.
///
/// `map.*` is a separate bundle from `app.*` because Leaflet is about as large again as
/// everything else and only one panel needs it.
pub const ASSETS: &[Asset] = &[
    Asset {
        name: "app.css",
        content_type: "text/css; charset=utf-8",
        bytes: include_bytes!("../static/app.css"),
    },
    Asset {
        name: "app.js",
        content_type: "text/javascript; charset=utf-8",
        bytes: include_bytes!("../static/app.js"),
    },
    Asset {
        name: "map.css",
        content_type: "text/css; charset=utf-8",
        bytes: include_bytes!("../static/map.css"),
    },
    Asset {
        name: "map.js",
        content_type: "text/javascript; charset=utf-8",
        bytes: include_bytes!("../static/map.js"),
    },
];

/// Find an asset by the name it is requested as.
///
/// A linear scan over four entries, which is faster than hashing the name would be and
/// needs no map to build at startup. The comparison is exact: there is no path to traverse,
/// no directory to escape and nothing to normalise, which is the other reason to embed these
/// rather than serve a directory.
#[must_use]
pub fn find(name: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|asset| asset.name == name)
}

/// A cache validator for the whole asset set.
///
/// One tag for all four, derived from their contents, so a browser revalidates everything
/// together after a rebuild and nothing at all in between. Deriving it from the bytes rather
/// than from the version means a development build that changes an asset without bumping the
/// version still invalidates — which is exactly when a stale cache wastes the most time.
///
/// FNV-1a over the concatenated contents: not a cryptographic hash, and it does not need to
/// be. It answers "did these bytes change", where the adversary is a browser cache.
#[must_use]
pub fn etag() -> &'static str {
    static ETAG: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ETAG.get_or_init(|| {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for asset in ASSETS {
            for byte in asset.bytes {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        format!("\"{hash:016x}\"")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// If the npm build stopped producing one of these, this crate would not compile — so
    /// this checks the weaker thing that can still go wrong: an entry pointing at a file
    /// that exists but is empty.
    #[test]
    fn every_asset_has_content() {
        assert_eq!(ASSETS.len(), 4);
        for asset in ASSETS {
            assert!(!asset.bytes.is_empty(), "{} is empty", asset.name);
            assert!(!asset.content_type.is_empty());
        }
    }

    #[test]
    fn assets_are_found_by_name() {
        assert!(find("app.css").is_some());
        assert!(find("map.js").is_some());
        assert!(find("nothing.css").is_none());
    }

    /// There is no filesystem behind this, so traversal is not a risk — but a future change
    /// that reintroduced one would be caught here rather than in the wild.
    #[test]
    fn nothing_resembling_a_path_resolves() {
        for name in [
            "../Cargo.toml",
            "../../../etc/passwd",
            "/etc/passwd",
            "app.css/../app.css",
            "APP.CSS",
            "",
        ] {
            assert!(find(name).is_none(), "{name:?} resolved to an asset");
        }
    }

    #[test]
    fn the_etag_is_stable_and_quoted() {
        let first = etag();
        assert_eq!(first, etag(), "the tag must not change between calls");
        assert!(first.starts_with('"') && first.ends_with('"'));
        assert!(first.len() > 2);
    }

    /// The CSS and JS have to actually be what they claim, or a browser refuses them under
    /// `X-Content-Type-Options: nosniff`.
    #[test]
    fn the_stylesheet_looks_like_a_stylesheet() {
        let css = find("app.css").expect("app.css is embedded");
        let text = std::str::from_utf8(css.bytes).expect("CSS is UTF-8");
        assert!(text.contains("tailwindcss"), "not the Tailwind build");
        assert!(text.contains("light-dark("), "the theme block is missing");
    }

    #[test]
    fn the_bundle_looks_like_the_bundle() {
        let js = find("app.js").expect("app.js is embedded");
        let text = std::str::from_utf8(js.bytes).expect("JS is UTF-8");
        assert!(text.contains("htmx"), "HTMX is not bundled");
    }
}
