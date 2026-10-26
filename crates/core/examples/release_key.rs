//! Maintainer tool for signing releases (not shipped to users).
//!
//!   cargo run -p hfr-core --example release_key -- keygen <private-key-file>
//!       creates a new key pair; the private key (hex) goes to the file, the public key is printed. Put the public
//!       key into `PUBLIC_KEYS` in `crates/core/src/update.rs` and the file's content into the GitHub secret
//!       `HFR_UPDATE_KEY`; keep a backup somewhere safe.
//!
//!   HFR_UPDATE_KEY=<hex> cargo run -p hfr-core --example release_key -- sign <SHA256SUMS> <SHA256SUMS.sig>
//!       signs the checksum file (the release workflow does this).
//!
//!   cargo run -p hfr-core --example release_key -- verify <SHA256SUMS> <SHA256SUMS.sig>
//!       checks a signature against the keys built into this crate.

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signer, SigningKey};
use hfr_core::update::{to_hex, verify_signature, PUBLIC_KEYS};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["keygen", file] => {
            if std::path::Path::new(file).exists() {
                bail!("{file} already exists; refusing to overwrite a key");
            }
            let sk = SigningKey::generate(&mut rand_core::OsRng);
            std::fs::write(file, to_hex(&sk.to_bytes()))?;
            println!("private key written to {file} (keep it secret and back it up)");
            println!("public key: {}", to_hex(sk.verifying_key().as_bytes()));
        }
        ["sign", sums, out] => {
            let hex = std::env::var("HFR_UPDATE_KEY").context("set HFR_UPDATE_KEY to the private key (hex)")?;
            let seed: [u8; 32] = (0..hex.trim().len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex.trim()[i..i + 2], 16))
                .collect::<Result<Vec<u8>, _>>()?
                .try_into()
                .map_err(|_| anyhow::anyhow!("the key must be 32 bytes of hex"))?;
            let sk = SigningKey::from_bytes(&seed);
            let data = std::fs::read(sums)?;
            std::fs::write(out, to_hex(&sk.sign(&data).to_bytes()))?;
            println!("signed {sums} -> {out} (public key {})", to_hex(sk.verifying_key().as_bytes()));
        }
        ["verify", sums, sig] => {
            let data = std::fs::read(sums)?;
            verify_signature(&data, &std::fs::read_to_string(sig)?, PUBLIC_KEYS)?;
            println!("signature OK");
        }
        _ => bail!("usage: release_key keygen <file> | sign <sums> <sig> | verify <sums> <sig>"),
    }
    Ok(())
}
