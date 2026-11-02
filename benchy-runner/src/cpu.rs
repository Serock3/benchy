//! Whole-process CPU usage on the controller, normalized to one logical CPU.
use crate::checked_output;
use anyhow::{Context, Result, ensure};
use std::time::Instant;

pub struct ProcessCpu {
    pid: u32,
    clock_ticks: u64,
    before: Sample,
}

struct Sample {
    ticks: u64,
    observed_at: Instant,
}

impl ProcessCpu {
    pub async fn start(pid: u32) -> Result<Self> {
        let output = checked_output("getconf", ["CLK_TCK"]).await?;
        let clock_ticks = String::from_utf8(output.stdout)?.trim().parse()?;
        ensure!(clock_ticks > 0, "CLK_TCK must be positive");
        Ok(Self {
            pid,
            clock_ticks,
            before: sample(pid).await?,
        })
    }

    pub async fn finish(self) -> Result<f64> {
        let after = sample(self.pid).await?;
        percent(&self.before, &after, self.clock_ticks)
    }
}

async fn sample(pid: u32) -> Result<Sample> {
    let started = Instant::now();
    let stat = tokio::fs::read_to_string(format!("/proc/{pid}/stat"))
        .await
        .context("cannot read controller process CPU counters (Linux required)")?;
    let finished = Instant::now();
    Ok(Sample {
        ticks: ticks(&stat)?,
        observed_at: started + (finished - started) / 2,
    })
}

fn ticks(stat: &str) -> Result<u64> {
    let command_end = stat.rfind(") ").context("missing process command name")?;
    let mut fields = stat[command_end + 2..].split_ascii_whitespace();
    let user: u64 = fields.nth(11).context("missing user CPU time")?.parse()?;
    let system: u64 = fields.next().context("missing system CPU time")?.parse()?;
    user.checked_add(system)
        .context("process CPU time overflowed")
}

fn percent(before: &Sample, after: &Sample, clock_ticks: u64) -> Result<f64> {
    let elapsed = after
        .observed_at
        .checked_duration_since(before.observed_at)
        .context("process CPU samples are out of order")?;
    let ticks = after
        .ticks
        .checked_sub(before.ticks)
        .context("process CPU time moved backwards")?;
    ensure!(
        !elapsed.is_zero() && clock_ticks > 0,
        "empty process CPU interval"
    );
    Ok(ticks as f64 / clock_ticks as f64 / elapsed.as_secs_f64() * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_may_contain_spaces_and_parentheses() {
        assert_eq!(
            ticks("12 (a process) name) S 1 2 3 4 5 6 7 8 9 10 120 30 99").unwrap(),
            150
        );
        assert!(ticks("broken").is_err());
    }
    #[test]
    fn cpu_is_not_normalized_by_machine_core_count() {
        let now = Instant::now();
        let before = Sample {
            ticks: 100,
            observed_at: now,
        };
        let after = Sample {
            ticks: 500,
            observed_at: now + std::time::Duration::from_secs(2),
        };
        assert_eq!(percent(&before, &after, 100).unwrap(), 200.0);
        assert!(percent(&after, &before, 100).is_err());
        assert!(percent(&before, &before, 100).is_err());
    }
}
