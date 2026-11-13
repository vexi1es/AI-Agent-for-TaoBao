use std::process::Command;
use std::time::Duration;

const CLI: &str = r"C:\Program Files\Cloudflare\Cloudflare WARP\warp-cli.exe";

fn is_connected(status: &str) -> bool {
    status.lines().any(|l| {
        let l = l.trim();
        l.strip_prefix("Status update:").map_or(false, |s| s.trim() == "Connected")
    })
}

fn run(args: &[&str]) -> Option<String> {
    let out = Command::new(CLI).arg("--accept-tos").args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn installed() -> bool {
    std::path::Path::new(CLI).exists()
}

fn connected() -> bool {
    run(&["status"]).map_or(false, |s| is_connected(&s))
}

pub async fn ensure_on() -> anyhow::Result<bool> {
    if connected() {
        return Ok(false);
    }
    run(&["connect"]).ok_or_else(|| anyhow::anyhow!("warp-cli не запустился"))?;
    for _ in 0..30 {
        if connected() {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow::anyhow!("WARP не подключился за 30 секунд"))
}

pub fn off() {
    let _ = run(&["disconnect"]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_status() {
        assert!(is_connected("Status update: Connected\nNetwork: healthy\n"));
        assert!(!is_connected("Status update: Disconnected\nReason: Manual Disconnection\n"));
        assert!(!is_connected("Status update: Connecting\n"));
        assert!(!is_connected(""));
    }
}
