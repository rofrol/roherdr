//! Explicit local-owner credit ledger operations; not a client endpoint method.
const USAGE: &str = "usage: herdr credits status | herdr credits release <entry>\nRelease only after stopping the run and verifying the request consumed no tokens.";

pub(super) fn run(args: &[String]) -> std::io::Result<i32> {
    #[cfg(unix)]
    {
        let config = crate::config::Config::load().config;
        let path = crate::platform::anthropic_proxy::ledger_path(&config.workers)?;
        match args {
            [action] if action == "status" => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &crate::platform::anthropic_proxy::credit_status(&path)?
                    )?
                );
            }
            [action, entry] if action == "release" => {
                let entry: i64 = entry.parse().map_err(std::io::Error::other)?;
                if entry <= 0 {
                    return Err(std::io::Error::other("entry must be a positive integer"));
                }
                crate::platform::anthropic_proxy::credit_release(&path, entry)?;
                println!("Released credit entry {entry} on the owner's attestation of no usage.");
            }
            _ => return Err(std::io::Error::other(USAGE)),
        }
        Ok(0)
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        Err(std::io::Error::other(format!(
            "Anthropic credit workers require Unix; {USAGE}"
        )))
    }
}
