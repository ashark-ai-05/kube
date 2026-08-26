//! Keeping credential-plugin output off the screen, out of stderr, and out of
//! any log a terminal happens to be recording.
//!
//! v1 is read-only and never *reads* a credential deliberately — but it does
//! format `kube::Error`s into user-visible text on half a dozen paths, and one
//! family of those errors carries an exec plugin's raw stdout. A plugin that
//! prints an `ExecCredential` and then exits non-zero (a token that was minted
//! but rejected, a wrapper script that logs and fails) puts a live bearer token
//! inside the error value.
//!
//! **Why length-capping is not a mitigation.**
//! `kube::client::auth::Error::AuthExecRun` formats `out: std::process::Output`
//! with `{out:?}`. Before Rust 1.66 that rendered stdout as a decimal byte
//! array — unreadable, and the basis on which `main.rs`'s `truncate_error` was
//! originally justified. Rust 1.66 added a manual `Debug` for `Output` that
//! prints stdout and stderr **as strings** when they are valid UTF-8. Verified
//! on this repo's rustc 1.93.1:
//!
//! ```text
//! auth error: auth exec command 'kubelogin' failed with status exit status: 1:
//!   Output { status: ExitStatus(unix_wait_status(256)),
//!            stdout: "{\"kind\":\"ExecCredential\",\"status\":{\"token\":\"…\"}}",
//!            stderr: "" }
//! ```
//!
//! The prefix before the token is ~100 characters, so a 200-character cap
//! passes most of the token through. Redaction has to be by TYPE — recognising
//! that this error carries credential material at all — not by length over
//! content nobody controls.
//!
//! **The command name is not safe either.** `AuthExecRun.cmd` is built as
//! `format!("{cmd:?}")` over a `std::process::Command`
//! (kube-client 4.2.0 `auth/mod.rs:636`), and `Command`'s `Debug` prints the
//! process environment ahead of the program. Verified on rustc 1.93.1:
//!
//! ```text
//! AWS_SECRET_ACCESS_KEY="hunter2" KUBERNETES_EXEC_INFO="{…}" "kubelogin" "get-token"
//! ```
//!
//! So this module names nothing out of the error at all. Where the user needs
//! the plugin's name, it comes from the kubeconfig instead — see
//! `main.rs`'s `connect_failure_hint`, which reads `AuthMethod::Exec`.
//!
//! **Two error shapes reach us.** The client-construction path yields
//! `kube::Error::Auth(AuthError)` (`config_ext.rs:349`); the per-request lazy
//! refresh runs inside a tower `AsyncPredicate` whose failure is boxed, so it
//! arrives as `kube::Error::Service(Box<AuthError>)` (`auth/mod.rs:190-205`).
//! A `watcher::Error` wraps either one layer deeper again. All three were
//! probed: `std::error::Error::source()` reaches the `AuthError` in every case,
//! so walking the source chain and downcasting covers all of them without this
//! module having to know which layer wrapped which.

use kube::client::AuthError;

/// What the user sees in place of an auth error that may carry credentials.
///
/// Says what happened, says why the detail is missing (so it does not read as
/// a bug), and points at the one place the real output can be seen safely.
pub const CREDENTIALS_WITHHELD: &str = "credential plugin failed — its output is withheld here because it can contain a bearer \
     token; run your kubeconfig's exec command in a shell to see it";

/// Whether this auth error's `Display` may contain bytes the credential
/// plugin, the token endpoint or the process environment produced.
///
/// **Allowlist, not blocklist.** The default is to redact, and only variants
/// checked by hand against kube-client 4.2.0 to carry nothing but static text,
/// an `io::Error`, a path or a date are let through. A kube upgrade that adds
/// a variant therefore starts out redacted rather than starting out leaking,
/// which is the direction a mistake here should fail in. The cost of a false
/// positive is one less-specific error message; the cost of a false negative
/// is a live token on someone's screen.
///
/// `AuthExecStart` is deliberately allowed: it is the "plugin binary is not on
/// PATH" case, by far the most common exec failure and the one whose detail is
/// the entire answer.
fn may_carry_credentials(e: &AuthError) -> bool {
    !matches!(
        e,
        AuthError::InvalidBasicAuth(_)
            | AuthError::InvalidBearerToken(_)
            | AuthError::UnrefreshableTokenResponse
            | AuthError::ExecPluginFailed
            | AuthError::MalformedTokenExpirationDate(_)
            | AuthError::AuthExecStart(_)
            | AuthError::ReadTokenFile(_, _)
            | AuthError::MissingCommand
            | AuthError::ExecMissingClusterInfo
            | AuthError::NoValidNativeRootCA(_)
    )
}

/// A replacement message if `err`'s chain carries credential material, else
/// `None`.
///
/// Walks `source()` from `err` itself downwards rather than matching one
/// expected nesting: the same `AuthError` reaches us as
/// `kube::Error::Auth(_)`, as `kube::Error::Service(Box<_>)`, and inside a
/// `watcher::Error` wrapping either — all three verified by probe. Downcasting
/// at every depth means no call site has to know which shape it is holding.
pub fn redact_credential_source(err: &(dyn std::error::Error + 'static)) -> Option<String> {
    let mut node = Some(err);
    while let Some(e) = node {
        if let Some(auth) = e.downcast_ref::<AuthError>()
            && may_carry_credentials(auth)
        {
            return Some(CREDENTIALS_WITHHELD.to_string());
        }
        node = e.source();
    }
    None
}

/// The `anyhow` form of `redact_credential_source`.
///
/// `anyhow::Error::chain()` is the same `source()` walk, starting at the
/// innermost `anyhow` context — so a `kube::Error` that was given context on
/// the way up is still reached.
pub fn redact_credential_error(err: &anyhow::Error) -> Option<String> {
    err.chain()
        .find_map(|e| e.downcast_ref::<AuthError>())
        .filter(|auth| may_carry_credentials(auth))
        .map(|_| CREDENTIALS_WITHHELD.to_string())
}

/// `{err:#}`, unless that would print credential material — then the redaction.
///
/// This is the function every path that formats an `anyhow::Error` into
/// user-visible text must use. Formatting one directly is what the leak WAS.
pub fn safe_error_text(err: &anyhow::Error) -> String {
    redact_credential_error(err).unwrap_or_else(|| format!("{err:#}"))
}

/// `{err}`, unless that would print credential material — then the redaction.
///
/// The non-`anyhow` counterpart, for the paths holding a concrete error type:
/// `watcher::Error` in `store::watch`, `kube::Error` in `cluster::namespaces`.
pub fn safe_source_text(err: &(dyn std::error::Error + 'static)) -> String {
    redact_credential_source(err).unwrap_or_else(|| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    /// The token a real plugin would have printed to stdout before failing.
    /// Distinctive enough that finding it anywhere in an output string is
    /// unambiguous.
    const TOKEN: &str = "SUPER-SECRET-TOKEN-abc123";

    /// An `AuthExecRun` shaped exactly as kube-client builds one: an
    /// `ExecCredential` on stdout, a non-zero exit, and a `cmd` string that —
    /// like the real one — is `Command`'s `Debug` output complete with
    /// environment variables.
    fn auth_exec_run() -> AuthError {
        AuthError::AuthExecRun {
            cmd: "AWS_SECRET_ACCESS_KEY=\"hunter2\" \"kubelogin\" \"get-token\"".to_string(),
            status: ExitStatus::from_raw(256),
            out: Output {
                status: ExitStatus::from_raw(256),
                stdout: format!(r#"{{"kind":"ExecCredential","status":{{"token":"{TOKEN}"}}}}"#)
                    .into_bytes(),
                stderr: Vec::new(),
            },
        }
    }

    /// Proof the fixture is not vacuous: the unredacted rendering of every
    /// shape under test really does contain the token in readable plaintext.
    /// If Rust ever reverts `Output`'s `Debug` to a byte array this test fails
    /// and the module's premise can be re-examined, rather than the redaction
    /// silently guarding nothing.
    #[test]
    fn the_unredacted_rendering_really_does_leak_the_token() {
        let direct = kube::Error::Auth(auth_exec_run());
        assert!(
            direct.to_string().contains(TOKEN),
            "kube::Error::Auth must leak in plaintext, or this module guards nothing: {direct}"
        );

        let via_service = kube::Error::Service(Box::new(auth_exec_run()));
        assert!(
            via_service.to_string().contains(TOKEN),
            "kube::Error::Service must leak in plaintext too: {via_service}"
        );

        let watched = kube::runtime::watcher::Error::WatchStartFailed(kube::Error::Service(
            Box::new(auth_exec_run()),
        ));
        assert!(
            watched.to_string().contains(TOKEN),
            "a watcher::Error wrapping one must leak too: {watched}"
        );

        let anyhowed = anyhow::Error::new(kube::Error::Auth(auth_exec_run()))
            .context("connecting to a cluster");
        assert!(
            format!("{anyhowed:#}").contains(TOKEN),
            "the anyhow alternate form must leak too"
        );
    }

    #[test]
    fn a_direct_auth_error_is_redacted() {
        let e = anyhow::Error::new(kube::Error::Auth(auth_exec_run()));
        let text = safe_error_text(&e);
        assert!(!text.contains(TOKEN), "the token must not survive: {text}");
        assert_eq!(text, CREDENTIALS_WITHHELD);
    }

    #[test]
    fn an_auth_error_boxed_by_the_tower_layer_is_redacted() {
        // The per-request lazy refresh path: the auth failure is boxed by the
        // `AsyncPredicate` and surfaces as `Service`, not `Auth`. A matcher
        // that only looked for `kube::Error::Auth(_)` would miss the shape
        // that fires most often, because exec credentials refresh per request
        // rather than only at connect.
        let e = anyhow::Error::new(kube::Error::Service(Box::new(auth_exec_run())));
        let text = safe_error_text(&e);
        assert!(!text.contains(TOKEN), "the token must not survive: {text}");
        assert_eq!(text, CREDENTIALS_WITHHELD);
    }

    #[test]
    fn context_added_above_the_auth_error_does_not_hide_it() {
        let e = anyhow::Error::new(kube::Error::Auth(auth_exec_run()))
            .context("discovering kinds")
            .context("starting up");
        let text = safe_error_text(&e);
        assert!(!text.contains(TOKEN), "the token must not survive: {text}");
    }

    #[test]
    fn a_watcher_error_wrapping_one_is_redacted() {
        let e = kube::runtime::watcher::Error::WatchStartFailed(kube::Error::Service(Box::new(
            auth_exec_run(),
        )));
        let text = safe_source_text(&e);
        assert!(!text.contains(TOKEN), "the token must not survive: {text}");
        assert_eq!(text, CREDENTIALS_WITHHELD);
    }

    #[test]
    fn the_environment_in_the_command_field_is_withheld_too() {
        // `AuthExecRun.cmd` is `Command`'s Debug, which prints env vars ahead
        // of the program — so "name the command but not the output" would
        // still leak. Nothing from the error is echoed.
        let e = anyhow::Error::new(kube::Error::Auth(auth_exec_run()));
        let text = safe_error_text(&e);
        assert!(
            !text.contains("hunter2"),
            "an env var carried in `cmd` must not survive either: {text}"
        );
    }

    #[test]
    fn an_error_that_carries_no_credentials_is_passed_through_verbatim() {
        // The redaction must not swallow ordinary errors: an apiserver 403 is
        // the single most useful message this tool shows, and a matcher that
        // redacted everything would be indistinguishable from one that worked
        // in the leak tests above.
        let e = anyhow::Error::new(kube::Error::Api(Box::new(kube::core::Status {
            code: 403,
            message: "pods is forbidden".to_string(),
            reason: "Forbidden".to_string(),
            ..Default::default()
        })));
        let text = safe_error_text(&e);
        assert!(
            text.contains("pods is forbidden"),
            "a non-credential error must survive intact: {text}"
        );
        assert_eq!(redact_credential_error(&e), None);
    }

    #[test]
    fn a_plugin_binary_that_is_not_on_path_still_says_so() {
        // `AuthExecStart` carries an io::Error and no plugin output, and is
        // the most common exec failure of all. Redacting it would trade a
        // credential leak for a mystery.
        let e = anyhow::Error::new(kube::Error::Auth(AuthError::AuthExecStart(
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such file or directory"),
        )));
        let text = safe_error_text(&e);
        assert!(
            text.contains("no such file or directory"),
            "the 'plugin not installed' detail must survive: {text}"
        );
    }

    #[test]
    fn a_parse_failure_over_plugin_stdout_is_redacted() {
        // `ParseTokenKey` wraps a deserialiser error over the plugin's stdout,
        // and deserialisers routinely quote the offending input — so it is on
        // the redacted side of the allowlist even though it is not
        // `AuthExecRun`.
        let Err(inner) = serde_json::from_str::<serde_json::Value>(r#"{"token":"tok""#) else {
            panic!("the fixture must actually fail to parse");
        };
        let e = anyhow::Error::new(kube::Error::Auth(AuthError::ParseTokenKey(inner)));
        assert_eq!(
            redact_credential_error(&e),
            Some(CREDENTIALS_WITHHELD.into())
        );
    }
}
