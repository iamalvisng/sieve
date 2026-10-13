//! Phase timing for a build or a refresh. `SIEVE_TIMING=1` prints one
//! line per phase to stderr. Without the variable the timer prints nothing.

use std::time::{Duration, Instant};

/// Times the phases of one run, one lap at a time.
pub struct Timer {
    on: bool,
    last: Instant,
}

impl Timer {
    /// Starts a timer. The variable is read once.
    pub fn start() -> Self {
        let on = std::env::var("SIEVE_TIMING").is_ok_and(|v| !v.is_empty() && v != "0");
        Timer {
            on,
            last: Instant::now(),
        }
    }

    /// Prints the time since the last lap under `name`, then restarts.
    pub fn lap(&mut self, name: &str) {
        let now = Instant::now();
        self.show(name, now - self.last);
        self.last = now;
    }

    /// Prints a total that the caller summed, such as the read time of all files.
    pub fn show(&self, name: &str, spent: Duration) {
        if self.on {
            eprintln!("timing {name}: {:.1} ms", spent.as_secs_f64() * 1000.0);
        }
    }
}
