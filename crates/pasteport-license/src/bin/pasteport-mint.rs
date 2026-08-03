//! Vendor-side tool: generate a signing keypair and sign license keys.
//!
//! Built only with `--features mint`, so it cannot be compiled out of a release
//! artifact by accident.
//!
//! ```text
//! pasteport-mint keygen
//! PASTEPORT_SIGNING_SEED=<seed> pasteport-mint sign --email a@b.co --plan personal --days 365
//! ```

use std::process::ExitCode;

use pasteport_license::mint::Minter;
use pasteport_license::{LicensePayload, Plan};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("keygen") => keygen(),
        Some("sign") => match sign(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
pasteport-mint — Pasteport license signing

USAGE:
    pasteport-mint keygen
    pasteport-mint sign --email <EMAIL> [OPTIONS]

SIGN OPTIONS:
    --email <EMAIL>     Licensee email (required)
    --plan <PLAN>       personal | team | lifetime  [default: personal]
    --seats <N>         Seats, for team plans       [default: 1]
    --days <N>          Term length. Omit for a perpetual key
    --id <ID>           License id. Generated if omitted

ENVIRONMENT:
    PASTEPORT_SIGNING_SEED   base64url signing seed, required by `sign`
";

fn keygen() -> ExitCode {
    // Derive a fresh seed from OS entropy.
    let seed = match random_seed() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not read entropy: {e}");
            return ExitCode::FAILURE;
        }
    };
    let minter = Minter::from_seed(&seed);

    println!("seed (SECRET, keep offline):       {}", minter.seed_b64());
    println!(
        "public key (compile into builds):  {}",
        minter.verifying_key_b64()
    );
    println!();
    println!("Build releases with:");
    println!(
        "  PASTEPORT_LICENSE_PUBKEY={} cargo build --release",
        minter.verifying_key_b64()
    );
    ExitCode::SUCCESS
}

fn sign(args: &[String]) -> Result<(), String> {
    let seed_b64 = std::env::var("PASTEPORT_SIGNING_SEED")
        .map_err(|_| "PASTEPORT_SIGNING_SEED is not set".to_string())?;
    let minter = Minter::from_seed_b64(&seed_b64).map_err(|e| e.to_string())?;

    let mut email = None;
    let mut plan = Plan::Personal;
    let mut seats: u32 = 1;
    let mut days: Option<i64> = None;
    let mut id = None;

    let mut i = 0;
    while i < args.len() {
        let flag = args[i].clone();
        let value = args
            .get(i + 1)
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))?;

        match flag.as_str() {
            "--email" => email = Some(value),
            "--plan" => {
                plan = match value.as_str() {
                    "personal" => Plan::Personal,
                    "team" => Plan::Team,
                    "lifetime" => Plan::Lifetime,
                    other => return Err(format!("unknown plan {other:?}")),
                }
            }
            "--seats" => {
                seats = value
                    .parse()
                    .map_err(|_| "--seats must be a number".to_string())?
            }
            "--days" => {
                days = Some(
                    value
                        .parse()
                        .map_err(|_| "--days must be a number".to_string())?,
                )
            }
            "--id" => id = Some(value),
            other => return Err(format!("unknown flag {other:?}")),
        }
        i += 2;
    }

    let email = email.ok_or("--email is required")?;
    let issued_at = time::OffsetDateTime::now_utc().unix_timestamp();
    let id = match id {
        Some(id) => id,
        None => generate_id().map_err(|e| e.to_string())?,
    };

    let payload = LicensePayload {
        v: 1,
        id,
        email,
        plan,
        seats: if plan == Plan::Team { seats.max(1) } else { 1 },
        issued_at,
        expires_at: days.map(|d| issued_at + d * 86_400),
    };

    let key = minter.sign(&payload).map_err(|e| e.to_string())?;
    println!("{key}");
    eprintln!();
    eprintln!("  id       {}", payload.id);
    eprintln!("  email    {}", payload.email);
    eprintln!("  plan     {}", payload.plan.as_str());
    eprintln!("  seats    {}", payload.seats);
    eprintln!(
        "  expires  {}",
        payload
            .expires_at
            .map_or("never".to_string(), |t| t.to_string())
    );
    Ok(())
}

fn random_seed() -> std::io::Result<[u8; 32]> {
    use std::io::Read as _;
    let mut file = std::fs::File::open("/dev/urandom")?;
    let mut seed = [0u8; 32];
    file.read_exact(&mut seed)?;
    Ok(seed)
}

fn generate_id() -> std::io::Result<String> {
    let seed = random_seed()?;
    let hex: String = seed.iter().take(6).map(|b| format!("{b:02x}")).collect();
    Ok(format!("lic_{hex}"))
}
