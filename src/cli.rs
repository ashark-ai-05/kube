//! CLI argument parsing for namespace selection.
//!
//! Supports the following flags:
//! - `-n <namespace>` / `--namespace <namespace>` to watch a single namespace
//! - `-A` / `--all-namespaces` to watch all namespaces
//! - `-h` / `--help` to show usage
//! - No arguments falls back to the kubeconfig context's namespace (or "default")
//!
//! When both `-n` and `-A` are provided, `-A` takes precedence (it is the more
//! explicit "all namespaces" declaration).

/// Which namespace scope to watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceScope {
    /// Watch a single namespace.
    One(String),
    /// Watch every namespace the user can see.
    All,
    /// Fall back to the kubeconfig context's namespace, else "default".
    FromContext,
}

/// The result of parsing CLI arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliOutcome {
    /// Successfully parsed; run with this scope.
    Run(NamespaceScope),
    /// User requested help (--help or -h).
    Help,
    /// An error occurred; print this message to stderr and exit 2.
    Error(String),
}

/// Decide whether to show the "try -A for all namespaces" hint.
///
/// Shows the hint when the namespace was chosen via the default fallback (not
/// explicitly set in kubeconfig or via `-n` flag) AND the watch has zero items.
/// This helps users understand why their table appears empty when they're watching
/// the "default" namespace on a cluster where default is empty.
pub fn should_hint_all_namespaces(was_fallback: bool, item_count: usize) -> bool {
    was_fallback && item_count == 0
}

/// Parse command-line arguments for namespace selection.
///
/// Takes an iterator of string arguments (typically `std::env::args().skip(1)`).
/// Returns a `CliOutcome` describing what action to take.
///
/// # Behavior
/// - No arguments: `Run(FromContext)`
/// - `-h` or `--help`: `Help`
/// - `-n <value>` or `--namespace <value>`: `Run(One(value))`
/// - `-A` or `--all-namespaces`: `Run(All)`
/// - `-n` or `--namespace` without a value: `Error(...)`
/// - Unknown flag: `Error(...)`
/// - Both `-n` and `-A`: `-A` wins; returns `Run(All)`
pub fn parse_args<I, S>(args: I) -> CliOutcome
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args_iter = args.into_iter();
    let mut namespace: Option<String> = None;
    let mut all_namespaces = false;

    while let Some(arg) = args_iter.next() {
        let arg_str = arg.as_ref();

        match arg_str {
            "-h" | "--help" => return CliOutcome::Help,
            "-A" | "--all-namespaces" => {
                all_namespaces = true;
            }
            "-n" | "--namespace" => match args_iter.next() {
                Some(ns) => {
                    namespace = Some(ns.as_ref().to_string());
                }
                None => {
                    let flag = if arg_str == "-n" { "-n" } else { "--namespace" };
                    return CliOutcome::Error(format!("flag {} requires a value", flag));
                }
            },
            _ => {
                return CliOutcome::Error(format!("unknown flag: {}", arg_str));
            }
        }
    }

    // If -A is provided, it takes precedence over -n
    if all_namespaces {
        CliOutcome::Run(NamespaceScope::All)
    } else if let Some(ns) = namespace {
        CliOutcome::Run(NamespaceScope::One(ns))
    } else {
        CliOutcome::Run(NamespaceScope::FromContext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_gives_from_context() {
        let args: Vec<&str> = vec![];
        assert_eq!(
            parse_args(args),
            CliOutcome::Run(NamespaceScope::FromContext)
        );
    }

    #[test]
    fn short_namespace_flag_gives_one() {
        let args = vec!["-n", "payments"];
        assert_eq!(
            parse_args(args),
            CliOutcome::Run(NamespaceScope::One("payments".into()))
        );
    }

    #[test]
    fn long_namespace_flag_gives_one() {
        let args = vec!["--namespace", "payments"];
        assert_eq!(
            parse_args(args),
            CliOutcome::Run(NamespaceScope::One("payments".into()))
        );
    }

    #[test]
    fn short_all_flag_gives_all() {
        let args = vec!["-A"];
        assert_eq!(parse_args(args), CliOutcome::Run(NamespaceScope::All));
    }

    #[test]
    fn long_all_flag_gives_all() {
        let args = vec!["--all-namespaces"];
        assert_eq!(parse_args(args), CliOutcome::Run(NamespaceScope::All));
    }

    #[test]
    fn help_short_flag_gives_help() {
        let args = vec!["-h"];
        assert_eq!(parse_args(args), CliOutcome::Help);
    }

    #[test]
    fn help_long_flag_gives_help() {
        let args = vec!["--help"];
        assert_eq!(parse_args(args), CliOutcome::Help);
    }

    #[test]
    fn namespace_flag_without_value_is_error() {
        let args = vec!["-n"];
        match parse_args(args) {
            CliOutcome::Error(msg) => {
                assert!(msg.contains("-n"), "error should name the flag");
            }
            other => panic!("expected Error, got: {:?}", other),
        }
    }

    #[test]
    fn long_namespace_flag_without_value_is_error() {
        let args = vec!["--namespace"];
        match parse_args(args) {
            CliOutcome::Error(msg) => {
                assert!(msg.contains("--namespace"), "error should name the flag");
            }
            other => panic!("expected Error, got: {:?}", other),
        }
    }

    #[test]
    fn unknown_flag_is_error() {
        let args = vec!["--nope"];
        match parse_args(args) {
            CliOutcome::Error(msg) => {
                assert!(msg.contains("--nope"), "error should name the unknown flag");
            }
            other => panic!("expected Error, got: {:?}", other),
        }
    }

    #[test]
    fn all_takes_precedence_when_combined_with_namespace() {
        let args = vec!["-n", "payments", "-A"];
        assert_eq!(
            parse_args(args),
            CliOutcome::Run(NamespaceScope::All),
            "when both -n and -A are provided, -A wins (it is the more explicit intent)"
        );
    }

    #[test]
    fn all_takes_precedence_even_if_namespace_comes_last() {
        let args = vec!["-A", "-n", "payments"];
        assert_eq!(
            parse_args(args),
            CliOutcome::Run(NamespaceScope::All),
            "-A always wins over -n regardless of order"
        );
    }

    #[test]
    fn hint_shows_only_on_fallback_with_zero_items() {
        // Hint should show: fallback to default, no items
        assert!(should_hint_all_namespaces(true, 0));

        // No hint: fallback but there are items (user knows namespace is not empty)
        assert!(!should_hint_all_namespaces(true, 1));
        assert!(!should_hint_all_namespaces(true, 100));

        // No hint: not a fallback, even with zero items (user explicitly chose this namespace)
        assert!(!should_hint_all_namespaces(false, 0));

        // No hint: not a fallback and has items
        assert!(!should_hint_all_namespaces(false, 1));
    }
}
