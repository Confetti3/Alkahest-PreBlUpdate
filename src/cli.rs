use build_time::build_time_utc;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use glam::Vec4;

pub const ALKAHEST_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUILD_DATE: &str = build_time_utc!("%Y-%m-%d");
pub const GIT_HASH: &str = env!("GIT_HASH");

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None, disable_version_flag(true))]
pub struct AppArgs {
    /// Shadowkeep client directory; its packages child directory will be used.
    #[arg(short, long, global = true, conflicts_with = "packages")]
    pub gamedir: Option<String>,

    /// Explicit Shadowkeep packages directory.
    #[arg(long, global = true, conflicts_with = "gamedir")]
    pub packages: Option<String>,

    /// What display the window should be on
    #[arg(long)]
    pub display: Option<usize>,

    #[arg(long)]
    pub open_map: Option<String>,

    /// Open a tag in the universal structural inspector at launch.
    #[arg(long)]
    pub open_tag: Option<String>,

    /// Open an activity tag in the universal structural inspector at launch.
    #[arg(long)]
    pub open_activity: Option<String>,

    #[arg(long)]
    pub test_scene: bool,

    /// Do not initialize the package-dependent 3D renderer. The catalog and
    /// structural inspector remain available.
    #[arg(long)]
    pub no_3d: bool,

    /// GPU policy used by GUI and capture modes.
    #[arg(long, value_enum, default_value_t = AdapterPreference::Auto)]
    pub adapter: AdapterPreference,

    #[command(subcommand)]
    pub command: Option<AppCommand>,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterPreference {
    Auto,
    Hardware,
    Warp,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AppCommand {
    /// Export universal structural inspections without creating a window.
    Export {
        #[command(subcommand)]
        target: ExportTarget,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ExportTarget {
    /// Export one tag inspection.
    Tag {
        tag: String,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Export one activity tag inspection.
    Activity {
        tag: String,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Export every package entry as separate JSON documents.
    All {
        #[arg(long)]
        output_dir: PathBuf,
    },
}

pub const BANNER: &str = r#"
    :::     :::        :::    :::     :::     :::    ::: :::::::::: :::::::: :::::::::::
  :+: :+:   :+:        :+:   :+:    :+: :+:   :+:    :+: :+:       :+:    :+:    :+:
 +:+   +:+  +:+        +:+  +:+    +:+   +:+  +:+    +:+ +:+       +:+           +:+
+#++:++#++: +#+        +#++:++    +#++:++#++: +#++:++#++ +#++:++#  +#++:++#++    +#+
+#+     +#+ +#+        +#+  +#+   +#+     +#+ +#+    +#+ +#+              +#+    +#+
#+#     #+# #+#        #+#   #+#  #+#     #+# #+#    #+# #+#       #+#    #+#    #+#
###     ### ########## ###    ### ###     ### ###    ### ########## ########     ###
"#;

pub const QUOTE: &str = r#"
    "Made possible by Clarity Control.
     Magnificent, wasn't it? An entity from beyond our own dimension.
     And the answer to humanity's eternal struggle: mortality"
        - Clovis Bray
"#;

fn text_gradient(a: Vec4, b: Vec4, text: &str) -> String {
    let mut result = String::new();

    result.push_str("\x1b[1m");
    result.push_str("\x1b[52m");

    let longest_line = text.lines().map(|l| l.len()).max().unwrap_or(0);

    for l in text.lines() {
        let mut gradient = String::new();
        for (i, c) in l.chars().enumerate() {
            let t = i as f32 / longest_line as f32;
            let color = a.lerp(b, t);

            // Background
            if c.is_whitespace() {
                gradient.push_str("\x1b[49m");
            } else {
                gradient.push_str(&format!(
                    "\x1b[48;2;{};{};{}m",
                    ((color.x * 0.1) * 255.) as u8,
                    ((color.y * 0.1) * 255.) as u8,
                    ((color.z * 0.1) * 255.) as u8,
                ));
            }

            // Foreground + character
            gradient.push_str(&format!(
                "\x1b[38;2;{};{};{}m{}",
                (color.x * 255.) as u8,
                (color.y * 255.) as u8,
                (color.z * 255.) as u8,
                c
            ));
        }
        result.push_str(&format!("{}\x1b[0m\n", gradient));
    }

    result.push_str("\x1b[0m");

    result
}

pub fn print_banner() {
    println!(
        "{}",
        text_gradient(
            Vec4::new(0.40, 0.40, 0.40, 1.00),
            Vec4::new(1.00, 0.55, 0.00, 1.00),
            BANNER
        )
    );
    println!(
        "                     \x1b[4mv{} ({} built on {})\x1b[0m",
        ALKAHEST_VERSION, GIT_HASH, BUILD_DATE
    );
    println!();
    println!("{}", QUOTE);
}
