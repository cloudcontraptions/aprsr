//! TLS for listeners and for uplinks.
//!
//! APRS-IS is a plaintext protocol carrying public data: positions and weather reports that
//! are broadcast over the air by design. TLS here is not about confidentiality of the
//! packets — it is about the *login line*, which carries a passcode, and about a client on a
//! hostile network being able to tell that the server it reached is the one it meant to.
//!
//! aprsr uses rustls rather than OpenSSL. Two reasons that matter for this project
//! specifically: it is the same TLS stack sea-orm already links, so the tree gains one
//! implementation rather than two; and it builds identically on Linux, macOS and Windows
//! without a system library to find, which is the difference between a cross-platform claim
//! and a cross-platform claim with an asterisk.
//!
//! ## Everything here is startup-time
//!
//! Certificates are loaded, parsed and validated once, when the server binds. A file that is
//! missing, unreadable or not a certificate is a startup failure with the path in the
//! message — never a connection that fails at three in the morning for a reason nobody can
//! reconstruct.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::{ClientConfig, RootCertStore, ServerConfig};
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// PEM parsing errors, from the crate rustls itself parses PEM with.
///
/// Reached through `rustls` rather than as a direct dependency: `rustls-pemfile`, the crate
/// this used to use, was archived in 2025 with the advice to depend on this code instead,
/// and it was only ever a thin wrapper around it. One fewer crate, and cargo-deny stays
/// quiet.
type PemError = tokio_rustls::rustls::pki_types::pem::Error;

/// Why TLS could not be set up.
///
/// Every variant names the file, because "invalid certificate" without a path is the least
/// useful message an operator with four of them can be given.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} contains no certificates")]
    NoCertificates { path: PathBuf },
    #[error("{path} contains no private key")]
    NoPrivateKey { path: PathBuf },
    #[error("{path} is not a usable private key: {source}")]
    BadPrivateKey {
        path: PathBuf,
        #[source]
        source: PemError,
    },
    #[error("the certificate in {cert} and the key in {key} do not go together: {source}")]
    Mismatched {
        cert: PathBuf,
        key: PathBuf,
        #[source]
        source: tokio_rustls::rustls::Error,
    },
    #[error("{path} contains no certificate authorities")]
    NoAuthorities { path: PathBuf },
}

/// Read every certificate from a PEM file.
///
/// A server certificate file normally holds the leaf followed by the intermediates that
/// chain it to a root — what Let's Encrypt calls `fullchain.pem`. All of them are kept and
/// sent, in order: a server that sends only its leaf works in a browser that already has the
/// intermediate cached and fails everywhere else, which is the classic TLS deployment bug and
/// the reason this reads the whole file rather than the first entry.
pub fn read_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let text = std::fs::read(path).map_err(|source| TlsError::Read {
        path: path.to_path_buf(),
        source,
    })?;

    // Malformed sections are skipped rather than failing the file. A PEM bundle from a
    // certificate authority can carry comment blocks and human-readable summaries between
    // the sections, and rejecting a usable chain because of one is not helpful. An empty
    // result is still an error, below.
    let certs: Vec<_> = CertificateDer::pem_slice_iter(&text)
        .filter_map(Result::ok)
        .collect();

    if certs.is_empty() {
        return Err(TlsError::NoCertificates {
            path: path.to_path_buf(),
        });
    }
    Ok(certs)
}

/// Read the first private key from a PEM file.
///
/// PKCS#8, PKCS#1 and SEC1 are all accepted, because all three are things an operator will
/// have been handed by a certificate authority or a tool and none of them is wrong.
pub fn read_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, TlsError> {
    let text = std::fs::read(path).map_err(|source| TlsError::Read {
        path: path.to_path_buf(),
        source,
    })?;

    PrivateKeyDer::from_pem_slice(&text).map_err(|source| match source {
        // "There is no key in this file" and "the key in this file is broken" are different
        // problems with different fixes — usually a path pointing at the certificate, versus
        // a truncated copy — so they stay different errors.
        PemError::NoItemsFound => TlsError::NoPrivateKey {
            path: path.to_path_buf(),
        },
        source => TlsError::BadPrivateKey {
            path: path.to_path_buf(),
            source,
        },
    })
}

/// Build the acceptor a TLS listener uses.
///
/// No client certificates are requested. APRS-IS authenticates with a passcode in the login
/// line, which every client in existence implements; requiring a certificate as well would
/// make a TLS port reachable by nothing.
pub fn acceptor(cert: &Path, key: &Path) -> Result<TlsAcceptor, TlsError> {
    let certificates = read_certificates(cert)?;
    let private_key = read_private_key(key)?;

    // This is where a mismatched pair is caught — at startup, with both paths in the
    // message, rather than as a handshake failure on the first connection.
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|source| TlsError::Mismatched {
            cert: cert.to_path_buf(),
            key: key.to_path_buf(),
            source,
        })?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Build the connector an uplink uses, trusting `ca_file` or the built-in roots.
///
/// The built-in roots are the Mozilla store, compiled in as data. Reading the operating
/// system's store instead would mean three different mechanisms across the three platforms
/// aprsr supports, and an uplink whose trust decisions depended on which one it was running
/// on — which is exactly the class of difference this project has been removing everywhere
/// else. An operator with a private CA points `ca_file` at it and gets that store *instead*,
/// not in addition: a private network that meant to trust one authority should not silently
/// keep trusting a hundred public ones.
pub fn connector(ca_file: Option<&Path>) -> Result<TlsConnector, TlsError> {
    let mut roots = RootCertStore::empty();

    match ca_file {
        Some(path) => {
            let authorities = read_certificates(path)?;
            let (added, _ignored) = roots.add_parsable_certificates(authorities);
            if added == 0 {
                return Err(TlsError::NoAuthorities {
                    path: path.to_path_buf(),
                });
            }
        }
        None => {
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
    }

    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    Ok(TlsConnector::from(Arc::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// The committed test certificates, in the integration tests' data directory so the two
    /// suites use the same ones rather than two sets that could drift apart. See
    /// `tests/data/generate.sh` for how they were made.
    fn test_data(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name)
    }

    fn temp_file(contents: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("a temporary file");
        file.write_all(contents).expect("writes");
        file.flush().expect("flushes");
        file
    }

    #[test]
    fn a_certificate_and_its_key_build_an_acceptor() {
        // `TlsAcceptor` has no `Debug`, so the result is matched rather than unwrapped.
        assert!(
            acceptor(&test_data("test-cert.pem"), &test_data("test-key.pem")).is_ok(),
            "the committed test pair is not usable"
        );
    }

    /// Every certificate in the file is read, not just the first.
    ///
    /// This is the deployment bug the function exists to avoid: a server that sends only its
    /// leaf works against a client that already has the intermediate cached and fails
    /// everywhere else. The committed `test-cert.pem` is the `fullchain.pem` shape — leaf,
    /// then the authority that signed it — so a regression to "first entry only" fails here.
    #[test]
    fn every_certificate_in_a_chain_file_is_read() {
        let certs = read_certificates(&test_data("test-cert.pem")).expect("reads");
        assert_eq!(certs.len(), 2, "the leaf and the authority behind it");
        assert!(certs.iter().all(|cert| !cert.is_empty()));
    }

    #[test]
    fn a_single_certificate_file_is_read_too() {
        let certs = read_certificates(&test_data("test-ca.pem")).expect("reads");
        assert_eq!(certs.len(), 1);
    }

    #[test]
    fn a_private_key_is_read_from_a_pem_file() {
        read_private_key(&test_data("test-key.pem")).expect("reads");
    }

    /// A missing file has to name itself. An operator with four certificate paths cannot act
    /// on "could not read certificate".
    #[test]
    fn a_missing_file_names_itself() {
        let missing = PathBuf::from("/nonexistent/aprsr/cert.pem");
        let error = read_certificates(&missing).expect_err("cannot read");
        assert!(matches!(error, TlsError::Read { .. }));
        assert!(error.to_string().contains("/nonexistent/aprsr/cert.pem"));
    }

    #[test]
    fn a_file_with_no_certificate_in_it_is_refused() {
        let file = temp_file(b"this is not a certificate\n");
        let error = read_certificates(file.path()).expect_err("no certificates");
        assert!(matches!(error, TlsError::NoCertificates { .. }));
    }

    #[test]
    fn a_file_with_no_key_in_it_is_refused() {
        let file = temp_file(b"# nothing here\n");
        let error = read_private_key(file.path()).expect_err("no key");
        assert!(matches!(error, TlsError::NoPrivateKey { .. }));
    }

    /// A key that is there but broken is a different problem from a key that is absent, and
    /// usually a different fix: a truncated copy, rather than a path pointing at the
    /// certificate. The two stay separate errors so the message points at the right one.
    #[test]
    fn a_corrupted_key_is_distinguished_from_a_missing_one() {
        let file = temp_file(
            b"-----BEGIN PRIVATE KEY-----\n\
              this is not base64 at all !!\n\
              -----END PRIVATE KEY-----\n",
        );
        let error = read_private_key(file.path()).expect_err("a broken key");
        assert!(
            matches!(error, TlsError::BadPrivateKey { .. }),
            "got {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains(&file.path().display().to_string())
        );
    }

    /// The pair is checked at startup, with both paths named — not as a handshake failure on
    /// the first connection at three in the morning.
    #[test]
    fn a_certificate_with_the_wrong_key_is_caught_at_startup() {
        // A syntactically valid but unrelated key: the certificate's own file, which parses
        // as neither.
        let Err(error) = acceptor(&test_data("test-cert.pem"), &test_data("test-cert.pem")) else {
            panic!("a certificate was accepted as a key");
        };
        assert!(
            matches!(error, TlsError::NoPrivateKey { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn a_connector_uses_the_built_in_roots_by_default() {
        assert!(
            connector(None).is_ok(),
            "the compiled-in root store is unusable"
        );
    }

    #[test]
    fn a_connector_can_trust_a_private_authority_instead() {
        assert!(
            connector(Some(&test_data("test-ca.pem"))).is_ok(),
            "a private CA file is unusable"
        );
    }

    /// A CA file with nothing usable in it must fail loudly. Falling back to the public
    /// roots would silently give a closed network the trust decisions it was configured to
    /// avoid.
    #[test]
    fn a_ca_file_with_no_authorities_is_refused() {
        let file = temp_file(b"not a certificate at all\n");
        let Err(error) = connector(Some(file.path())) else {
            panic!("a file with no authorities in it was accepted");
        };
        assert!(matches!(
            error,
            TlsError::NoCertificates { .. } | TlsError::NoAuthorities { .. }
        ));
    }

    #[test]
    fn every_error_names_a_path() {
        for error in [
            TlsError::NoCertificates {
                path: PathBuf::from("/tmp/a.pem"),
            },
            TlsError::NoPrivateKey {
                path: PathBuf::from("/tmp/b.pem"),
            },
            TlsError::NoAuthorities {
                path: PathBuf::from("/tmp/c.pem"),
            },
        ] {
            let rendered = error.to_string();
            assert!(rendered.contains("/tmp/"), "{rendered} names no path");
        }
    }
}
