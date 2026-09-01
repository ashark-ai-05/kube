use iced::Theme;
use kube_tui::cli::{CliOutcome, parse_args};
use kube_tui::gui::app::{Flags, KubeGui};

fn help_text() -> &'static str {
    "kube — native iced Kubernetes inspector (not affiliated with Lens, OpenLens, or k9s)

Usage:
  kube                 open the current kubeconfig context
  kube -n NAME         watch one namespace
  kube -A              watch all namespaces
  kube -h, --help

Requires a kubeconfig (KUBECONFIG or ~/.kube/config). Read-only.
"
}

fn main() -> iced::Result {
    match parse_args(std::env::args().skip(1)) {
        CliOutcome::Help => {
            print!("{}", help_text());
            Ok(())
        }
        CliOutcome::Error(msg) => {
            eprintln!("kube: {msg}");
            std::process::exit(2);
        }
        CliOutcome::Run(scope) => iced::application(KubeGui::title, KubeGui::update, KubeGui::view)
            .subscription(KubeGui::subscription)
            .theme(|_| Theme::Dark)
            .run_with(move || KubeGui::boot(Flags { scope })),
    }
}
