use std::time::Instant;

use colored::Colorize;

pub fn enabled() -> bool {
    matches!(
        std::env::var("RELAY_PERF_LOG").ok().as_deref(),
        Some(s) if s == "1" || s.eq_ignore_ascii_case("true")
    )
}

pub struct RelayPerf {
    scope: String,
    origin: Instant,
    seg: Instant,
    seq: u32,
}

impl RelayPerf {
    pub fn new(scope: impl Into<String>) -> Self {
        let t = Instant::now();
        Self {
            scope: scope.into(),
            origin: t,
            seg: t,
            seq: 0,
        }
    }

    pub fn step(&mut self, phase: &'static str) {
        let now = Instant::now();
        let dt_ms = now.duration_since(self.seg).as_secs_f64() * 1000.0;
        let cum_ms = now.duration_since(self.origin).as_secs_f64() * 1000.0;
        self.seg = now;
        self.seq += 1;
        let phase_c = match self.seq % 5 {
            0 => phase.bright_cyan(),
            1 => phase.bright_green(),
            2 => phase.bright_yellow(),
            3 => phase.bright_magenta(),
            _ => phase.bright_blue(),
        };
        let line = format!(
            "⏱ {} #{} {} Δ{:>7.2} ms │ cum{:>9.2} ms",
            self.scope.bright_white(),
            self.seq,
            phase_c,
            dt_ms,
            cum_ms
        );
        tracing::info!(target: "relay_perf", "{}", line);
    }
}

pub fn relay_perf(scope: impl Into<String>) -> Option<RelayPerf> {
    enabled().then(|| RelayPerf::new(scope))
}

pub fn step(p: &mut Option<RelayPerf>, label: &'static str) {
    if let Some(r) = p {
        r.step(label);
    }
}
