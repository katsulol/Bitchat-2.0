use std::process::Command;

fn run_git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn main() {
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-env-changed=BITCHAT_GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=BITCHAT_BUILD_DIRTY");

    let commit = std::env::var("BITCHAT_GIT_COMMIT")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| run_git(&["rev-parse", "--short=12", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_string());

    let dirty = std::env::var("BITCHAT_BUILD_DIRTY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            match Command::new("git")
                .args(["status", "--porcelain", "--untracked-files=no"])
                .output()
            {
                Ok(out) if out.status.success() => {
                    if out.stdout.iter().any(|b| !b.is_ascii_whitespace()) {
                        "true".to_string()
                    } else {
                        "false".to_string()
                    }
                }
                _ => "false".to_string(),
            }
        });

    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());

    println!("cargo:rustc-env=BITCHAT_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=BITCHAT_BUILD_DIRTY={dirty}");
    println!("cargo:rustc-env=BITCHAT_BUILD_TARGET={target}");
}
