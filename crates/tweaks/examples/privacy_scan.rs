//! Prints the state of every privacy setting on this PC. Read-only.
//!
//! `cargo run -p zest-tweaks --example privacy_scan`

use zest_tweaks::privacy::{Category, Privacy, State};
use zest_tweaks::tools::TOOLS;

fn main() {
    let privacy = Privacy::system();
    let windows = privacy.windows();
    println!(
        "Windows build {} ({})",
        windows.build,
        if windows.home { "Home" } else { "not Home" }
    );
    let list = privacy.list();
    for category in Category::ALL {
        println!("\n{}", category.label());
        for status in list.iter().filter(|s| s.tweak.category == category) {
            let state = match status.state {
                State::Private => "private",
                State::Default => "default",
                State::Custom => "custom",
                State::NotApplicable => "n/a",
            };
            println!(
                "  [{state:^7}] {:<48} {}{}{}",
                status.tweak.title,
                if status.tweak.recommended {
                    "recommended "
                } else {
                    ""
                },
                if status.needs_admin {
                    "needs-admin "
                } else {
                    ""
                },
                if status.edition_caveat {
                    "home-caveat"
                } else {
                    ""
                },
            );
        }
    }
    println!(
        "\nRecommended and not yet private: {}",
        privacy.recommended_pending().join(", ")
    );

    println!("\nWindows tools");
    for tool in TOOLS {
        println!(
            "  {:<26} {:<58} {}",
            tool.id,
            tool.command_line(),
            if tool.is_available() { "" } else { "MISSING" }
        );
    }
}
