use std::fs;

/// Keys that must be present at compile time (via `.env` file or real env vars).
const REQUIRED: &[&str] = &[
    "PERFETTO_GOOGLE_CLIENT_ID",
    "PERFETTO_GOOGLE_CLIENT_SECRET",
];

fn main() {
    // Load .env if it exists — real env vars take precedence.
    if let Ok(contents) = fs::read_to_string(".env") {
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim();
                if std::env::var(key).is_err() {
                    println!("cargo:rustc-env={key}={value}");
                }
            }
        }
    }

    // Ensure all required keys are available (from either source).
    for key in REQUIRED {
        if std::env::var(key).is_err() {
            // Check if we already emitted it from .env above by re-reading.
            // cargo:rustc-env sets it for rustc but not for this process,
            // so we need to check the .env contents directly.
            let found = fs::read_to_string(".env")
                .ok()
                .map(|c| {
                    c.lines().any(|l| {
                        let l = l.trim();
                        l.split_once('=')
                            .map(|(k, _)| k.trim() == *key)
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);

            if !found {
                panic!(
                    "\n\nMissing required env var: {key}\n\
                     Set it in .env (for local dev) or as an environment variable (for CI).\n"
                );
            }
        }
    }

    // Re-run if .env changes.
    println!("cargo:rerun-if-changed=.env");
    for key in REQUIRED {
        println!("cargo:rerun-if-env-changed={key}");
    }
}
