//! `pasteport` — the command-line client.
//!
//! Every command is one round trip to the daemon. Human-readable output by
//! default, `--json` for anything that needs to be parsed, which keeps the CLI
//! usable both at a prompt and from a script.

use std::path::PathBuf;

use anyhow::{bail, Context as _};
use clap::{Parser, Subcommand};

use pasteport_core::{paths, Clip, ClipKind};
use pasteport_daemon::protocol::{Request, Response};
use pasteport_daemon::Client;

#[derive(Debug, Parser)]
#[command(
    name = "pasteport",
    version,
    about = "Clipboard history for macOS and Linux",
    after_help = "The daemon must be running. Start it with `pasteportd`."
)]
struct Args {
    /// Control socket path. Defaults to the one inside the data directory.
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<PathBuf>,

    /// Data directory, if it is not the platform default.
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Emit JSON instead of formatted text.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show recent clips.
    #[command(visible_alias = "ls")]
    List {
        /// How many to show.
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        /// Skip this many.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Only this kind: text, rich-text, link, color, image, file.
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,
    },
    /// Search clip text.
    #[command(visible_alias = "s")]
    Search {
        query: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Print one clip's full text.
    Get { id: i64 },
    /// Put a stored clip back on the clipboard.
    #[command(visible_alias = "cp")]
    Copy { id: i64 },
    /// Pin a clip so it survives pruning.
    Pin { id: i64 },
    /// Unpin a clip.
    Unpin { id: i64 },
    /// Delete one clip.
    #[command(visible_alias = "rm")]
    Remove { id: i64 },
    /// Delete history.
    Clear {
        /// Also delete pinned clips and pinboard members.
        #[arg(long)]
        all: bool,
        /// Skip the confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Store whatever is on the clipboard right now.
    Capture,
    /// Apply the retention policy now.
    Prune,
    /// Daemon and history status.
    Status,
    /// Manage pinboards.
    #[command(subcommand)]
    Board(BoardCommand),
}

#[derive(Debug, Subcommand)]
enum BoardCommand {
    /// List pinboards.
    List,
    /// Create a pinboard.
    Create { name: String },
    /// Delete a pinboard. Clips themselves are kept.
    Delete { name: String },
    /// Add a clip to a pinboard.
    Add { name: String, id: i64 },
    /// Remove a clip from a pinboard.
    Remove { name: String, id: i64 },
    /// Show a pinboard's clips.
    Show {
        name: String,
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
    },
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    if let Some(dir) = &args.data_dir {
        std::env::set_var("PASTEPORT_DATA_DIR", dir);
    }
    let socket = match &args.socket {
        Some(p) => p.clone(),
        None => paths::socket_path()?,
    };

    // Confirmation happens before we bother the daemon.
    if let Command::Clear { all, yes } = &args.command {
        if !yes && !confirm_clear(*all)? {
            println!("Nothing was deleted.");
            return Ok(());
        }
    }

    let request = build_request(&args.command);

    let mut client = Client::connect(&socket).with_context(|| {
        format!(
            "could not reach the Pasteport daemon at {}.\nStart it with: pasteportd",
            socket.display()
        )
    })?;
    let response = client.request(&request)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        // A failed operation must still exit non-zero, even in JSON mode.
        if response.is_error() {
            std::process::exit(1);
        }
        return Ok(());
    }

    render(&args.command, &response)
}

fn build_request(command: &Command) -> Request {
    match command {
        Command::List {
            limit,
            offset,
            kind,
        } => Request::List {
            limit: *limit,
            offset: *offset,
            kind: kind.as_deref().and_then(parse_kind),
        },
        Command::Search { query, limit } => Request::Search {
            query: query.join(" "),
            limit: *limit,
        },
        Command::Get { id } => Request::Get { id: *id },
        Command::Copy { id } => Request::Copy { id: *id },
        Command::Pin { id } => Request::Pin {
            id: *id,
            pinned: true,
        },
        Command::Unpin { id } => Request::Pin {
            id: *id,
            pinned: false,
        },
        Command::Remove { id } => Request::Delete { id: *id },
        Command::Clear { all, .. } => Request::Clear {
            include_pinned: *all,
        },
        Command::Capture => Request::CaptureNow,
        Command::Prune => Request::Prune,
        Command::Status => Request::Status,
        Command::Board(BoardCommand::List) => Request::Pinboards,
        Command::Board(BoardCommand::Create { name }) => {
            Request::PinboardCreate { name: name.clone() }
        }
        Command::Board(BoardCommand::Delete { name }) => {
            Request::PinboardDelete { name: name.clone() }
        }
        Command::Board(BoardCommand::Add { name, id }) => Request::PinboardAdd {
            name: name.clone(),
            id: *id,
        },
        Command::Board(BoardCommand::Remove { name, id }) => Request::PinboardRemove {
            name: name.clone(),
            id: *id,
        },
        Command::Board(BoardCommand::Show { name, limit }) => Request::PinboardClips {
            name: name.clone(),
            limit: *limit,
        },
    }
}

/// Accept both `rich-text` and `rich_text`, since the CLI spells it with a dash
/// and the protocol with an underscore.
fn parse_kind(s: &str) -> Option<ClipKind> {
    ClipKind::parse(&s.trim().to_ascii_lowercase().replace('-', "_"))
}

fn render(command: &Command, response: &Response) -> anyhow::Result<()> {
    match response {
        Response::Error { message } => {
            eprintln!("Error: {message}");
            std::process::exit(1);
        }

        Response::Clips { clips } => {
            if clips.is_empty() {
                println!("No clips.");
            } else {
                print_clip_table(clips);
            }
        }

        Response::Clip { clip } => match command {
            // `get` prints the payload alone so it can be piped.
            Command::Get { .. } => match &clip.text {
                Some(text) => println!("{text}"),
                None => {
                    println!("{}", clip.preview(80));
                    eprintln!(
                        "(this clip is {}, {} bytes; use --json to fetch it)",
                        clip.mime, clip.byte_len
                    );
                }
            },
            Command::Copy { .. } => println!("Copied clip {}: {}", clip.id, clip.preview(60)),
            Command::Capture => println!("Captured clip {}: {}", clip.id, clip.preview(60)),
            Command::Pin { .. } => println!("Pinned clip {}", clip.id),
            Command::Unpin { .. } => println!("Unpinned clip {}", clip.id),
            _ => print_clip_table(std::slice::from_ref(clip.as_ref())),
        },

        Response::Pinboards { pinboards } => {
            if pinboards.is_empty() {
                println!("No pinboards. Create one with: pasteport board create <name>");
            } else {
                for board in pinboards {
                    let plural = if board.clip_count == 1 {
                        "clip"
                    } else {
                        "clips"
                    };
                    println!("{:<24} {} {plural}", board.name, board.clip_count);
                }
            }
        }

        Response::Count { count } => match command {
            Command::Clear { .. } => println!("Deleted {count} clips."),
            Command::Prune => println!("Pruned {count} clips."),
            _ => println!("{count}"),
        },

        Response::Status(report) => print_status(report),

        Response::Pong { version } => println!("pasteportd {version}"),
        Response::Bytes { base64 } => match base64 {
            Some(b64) => println!("{b64}"),
            None => println!("(no binary payload)"),
        },
        Response::Ok => println!("Done."),
    }
    Ok(())
}

fn print_clip_table(clips: &[Clip]) {
    // Width the id column to the widest id rather than guessing.
    let id_width = clips
        .iter()
        .map(|c| c.id.to_string().len())
        .max()
        .unwrap_or(2)
        .max(2);
    for clip in clips {
        let pin = if clip.pinned { "*" } else { " " };
        println!(
            "{pin}{:>id_width$}  {:<10}  {}",
            clip.id,
            clip.kind.as_str(),
            clip.preview(72),
            id_width = id_width
        );
    }
}

fn print_status(report: &pasteport_daemon::StatusReport) {
    let s = &report.stats;
    println!("Pasteport {}", report.version);
    println!("  backend        {}", report.backend);
    println!("  uptime         {}", format_duration(report.uptime_secs));
    println!("  poll interval  {} ms", report.poll_interval_ms);
    println!("  data dir       {}", report.data_dir);
    println!(
        "  clips          {} ({} pinned)",
        s.total_clips, s.pinned_clips
    );
    println!("  pinboards      {}", s.pinboards);
    println!("  stored         {}", format_bytes(s.total_bytes));
    println!(
        "  search         {}",
        if s.full_text_search {
            "full-text"
        } else {
            "substring"
        }
    );
}

fn format_duration(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        s if s < 86_400 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        s => format!("{}d {}h", s / 86_400, (s % 86_400) / 3600),
    }
}

fn format_bytes(bytes: i64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < KIB * KIB {
        format!("{:.1} KiB", b / KIB)
    } else if b < KIB * KIB * KIB {
        format!("{:.1} MiB", b / (KIB * KIB))
    } else {
        format!("{:.2} GiB", b / (KIB * KIB * KIB))
    }
}

/// Clearing history is not undoable, so it asks first unless told not to.
fn confirm_clear(all: bool) -> anyhow::Result<bool> {
    use std::io::Write as _;
    let what = if all {
        "ALL clips, including pinned ones and pinboard contents"
    } else {
        "all unpinned clips"
    };
    print!("Delete {what}? This cannot be undone. [y/N] ");
    std::io::stdout().flush()?;

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer)? == 0 {
        // Non-interactive with no input: refuse rather than assume yes.
        bail!("no confirmation received; re-run with --yes to clear non-interactively");
    }
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clip_kinds_in_both_spellings() {
        assert_eq!(parse_kind("link"), Some(ClipKind::Link));
        assert_eq!(parse_kind("rich-text"), Some(ClipKind::RichText));
        assert_eq!(parse_kind("rich_text"), Some(ClipKind::RichText));
        assert_eq!(parse_kind("  IMAGE  "), Some(ClipKind::Image));
        assert_eq!(parse_kind("nonsense"), None);
    }

    #[test]
    fn search_joins_multiple_words() {
        let req = build_request(&Command::Search {
            query: vec!["two".into(), "words".into()],
            limit: 5,
        });
        assert_eq!(
            req,
            Request::Search {
                query: "two words".into(),
                limit: 5
            }
        );
    }

    #[test]
    fn pin_and_unpin_map_to_one_request() {
        assert_eq!(
            build_request(&Command::Pin { id: 3 }),
            Request::Pin {
                id: 3,
                pinned: true
            }
        );
        assert_eq!(
            build_request(&Command::Unpin { id: 3 }),
            Request::Pin {
                id: 3,
                pinned: false
            }
        );
    }

    #[test]
    fn clear_all_asks_for_pinned_too() {
        assert_eq!(
            build_request(&Command::Clear {
                all: true,
                yes: true
            }),
            Request::Clear {
                include_pinned: true
            }
        );
        assert_eq!(
            build_request(&Command::Clear {
                all: false,
                yes: true
            }),
            Request::Clear {
                include_pinned: false
            }
        );
    }

    #[test]
    fn formats_durations_readably() {
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(90), "1m 30s");
        assert_eq!(format_duration(3_700), "1h 1m");
        assert_eq!(format_duration(90_000), "1d 1h");
    }

    #[test]
    fn formats_byte_counts_readably() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.00 GiB");
    }

    #[test]
    fn cli_definition_is_valid() {
        // clap panics on a malformed command tree, so this is a real check.
        use clap::CommandFactory as _;
        Args::command().debug_assert();
    }
}
