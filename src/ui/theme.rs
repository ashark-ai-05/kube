use ratatui::style::{Color, Modifier, Style};

// Chrome — the cool hue family. Borders, headers, labels, counts.
pub const INK: Color = Color::Rgb(0x0F, 0x11, 0x17);
pub const ABYSS: Color = Color::Rgb(0x15, 0x1A, 0x24);
pub const DUSK: Color = Color::Rgb(0x3A, 0x42, 0x60);
pub const INDIGO: Color = Color::Rgb(0x5B, 0x6E, 0xE1);
pub const PERIWINKLE: Color = Color::Rgb(0x8F, 0xA0, 0xFF);
pub const TEAL: Color = Color::Rgb(0x4F, 0xD6, 0xC9);
pub const VIOLET: Color = Color::Rgb(0xA7, 0x8B, 0xFA);

// Text.
pub const PAPER: Color = Color::Rgb(0xE4, 0xE8, 0xF0);
pub const MIST: Color = Color::Rgb(0x8A, 0x93, 0xA6);

// Signal — the warm-plus-green family. Data only, never chrome.
pub const VIRIDIAN: Color = Color::Rgb(0x3D, 0xD6, 0x8C);
pub const AMBER: Color = Color::Rgb(0xFF, 0xC1, 0x45);
pub const CORAL: Color = Color::Rgb(0xFF, 0x6B, 0x6B);

/// Curated cluster hues: fixed saturation and lightness so every cluster's
/// colour is equally legible. Hashing into raw RGB would eventually produce
/// something unreadable against the ground.
pub const CLUSTER_HUES: [Color; 10] = [
    Color::Rgb(0x5B, 0x6E, 0xE1), // indigo
    Color::Rgb(0x4F, 0xD6, 0xC9), // teal
    Color::Rgb(0xA7, 0x8B, 0xFA), // violet
    Color::Rgb(0x3D, 0xD6, 0x8C), // green
    Color::Rgb(0xFF, 0xC1, 0x45), // amber
    Color::Rgb(0xFF, 0x8F, 0xB1), // rose
    Color::Rgb(0x6B, 0xC5, 0xFF), // sky
    Color::Rgb(0xD6, 0xA5, 0x5B), // sand
    Color::Rgb(0x9B, 0xE5, 0x64), // lime
    Color::Rgb(0xFF, 0x9E, 0x64), // tangerine
];

/// A stable colour for a cluster, used by the ribbon and the context label.
///
/// FNV-1a: small, deterministic across runs and platforms, and good enough
/// for bucketing names into a fixed palette.
pub fn cluster_hue(name: &str) -> Color {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    CLUSTER_HUES[(hash % CLUSTER_HUES.len() as u64) as usize]
}

/// Colour a pod phase by severity so problems are visible without reading.
pub fn phase_style(phase: &str) -> Style {
    let color = match phase {
        "Running" | "Succeeded" | "Ready" | "Active" | "Bound" => VIRIDIAN,
        "Pending" | "ContainerCreating" | "PodInitializing" | "Terminating" => AMBER,
        "Failed" | "CrashLoopBackOff" | "Error" | "ImagePullBackOff" | "ErrImagePull"
        | "Evicted" | "OOMKilled" => CORAL,
        _ => MIST,
    };
    Style::default().fg(color)
}

pub fn border_style(focused: bool) -> Style {
    Style::default().fg(if focused { INDIGO } else { DUSK })
}

pub fn header_style() -> Style {
    Style::default().fg(PERIWINKLE).add_modifier(Modifier::BOLD)
}

pub fn label_style() -> Style {
    Style::default().fg(TEAL)
}

pub fn count_style() -> Style {
    Style::default().fg(VIOLET)
}

pub fn text_style() -> Style {
    Style::default().fg(PAPER)
}

pub fn muted_style() -> Style {
    Style::default().fg(MIST)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_hue_is_stable_for_the_same_name() {
        assert_eq!(cluster_hue("prod-eu"), cluster_hue("prod-eu"));
    }

    #[test]
    fn different_clusters_generally_get_different_hues() {
        // Not a guarantee for every pair — 20+ clusters into a finite palette
        // will collide — but the common case must discriminate.
        let names = ["prod-eu", "prod-us", "staging", "dev", "tst-wsdc"];
        let hues: std::collections::HashSet<_> = names.iter().map(|n| cluster_hue(n)).collect();
        assert!(
            hues.len() >= 4,
            "expected at least 4 distinct hues from 5 names, got {}",
            hues.len()
        );
    }

    #[test]
    fn every_cluster_hue_comes_from_the_curated_palette() {
        // Hashing to raw RGB would eventually produce an unreadable colour.
        for name in [
            "a",
            "b",
            "prod",
            "zzzz",
            "",
            "tst-wsdc",
            "a-very-long-cluster-name",
        ] {
            assert!(
                CLUSTER_HUES.contains(&cluster_hue(name)),
                "{name} produced a hue outside the curated palette"
            );
        }
    }

    #[test]
    fn failing_phases_are_visually_distinct_from_healthy_ones() {
        assert_ne!(phase_style("Running"), phase_style("CrashLoopBackOff"));
        assert_ne!(phase_style("Running"), phase_style("Pending"));
        assert_ne!(phase_style("Pending"), phase_style("CrashLoopBackOff"));
    }

    #[test]
    fn status_colours_never_reuse_a_chrome_token() {
        // Chrome is the cool family, signal is warm-plus-green. If a status
        // ever renders in a border colour it stops reading as a signal.
        let chrome = [INK, ABYSS, DUSK, INDIGO, PERIWINKLE, TEAL, VIOLET];
        for phase in [
            "Running",
            "Pending",
            "Failed",
            "CrashLoopBackOff",
            "Succeeded",
            "Unknown",
        ] {
            let fg = phase_style(phase)
                .fg
                .expect("phase styles must set a foreground");
            assert!(!chrome.contains(&fg), "{phase} rendered in a chrome colour");
        }
    }

    #[test]
    fn focus_is_visible_in_the_border() {
        assert_ne!(border_style(true), border_style(false));
    }
}
