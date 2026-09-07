//! Burns CPU on a repeating curve, so a dashboard's CPU column has a shape to
//! draw rather than a flat line.
//!
//! Most example apps sit near zero percent, which is honest and useless for
//! looking at anything that plots load over time. This one rises and falls on
//! a fixed period, so `shep lookout`'s sparkline draws a wave and two copies
//! with different arguments look different from each other.
//!
//! One thread, so it can never take more than a single core no matter what
//! `peak` says. Two of these on a fourteen-core machine is a rounding error in
//! the fans.
//!
//! # Usage
//!
//! ```text
//! busy <peak-percent> <period-secs>
//! ```
//!
//! `peak-percent` is the top of the curve as a percentage of one core, clamped
//! to 100. `period-secs` is how long one rise and fall takes.

#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

/// How often the duty cycle is recomputed. Short enough that the curve is
/// smooth at a two-second sampling interval, long enough that the arithmetic
/// itself is not the load.
const SLOT: Duration = Duration::from_millis(100);

fn main() {
    let mut args = std::env::args().skip(1);
    let peak = parse_percent(args.next().as_deref());
    let period = parse_period(args.next().as_deref());

    println!(
        "busy pid={} peaking at {peak}% of one core every {}s",
        std::process::id(),
        period.as_secs()
    );

    let start = Instant::now();
    let mut reported = 0_u64;
    loop {
        let phase = (start.elapsed().as_secs_f32() / period.as_secs_f32()).fract();
        // A cosine rather than a sawtooth: a sawtooth's vertical drop reads as
        // a missing sample on a sparkline, where a curve reads as a curve.
        let duty = f32::from(peak) / 100.0 * 0.5 * (1.0 - (phase * std::f32::consts::TAU).cos());

        let busy_for = SLOT.mul_f32(duty.clamp(0.0, 1.0));
        let slot_start = Instant::now();
        while slot_start.elapsed() < busy_for {
            std::hint::spin_loop();
        }
        std::thread::sleep(SLOT.saturating_sub(slot_start.elapsed()));

        let elapsed = start.elapsed().as_secs();
        if elapsed >= reported + 10 {
            reported = elapsed;
            println!("busy: {:.0}% of one core", duty * 100.0);
        }
    }
}

/// The peak as a percentage of one core, clamped to a single core's worth.
///
/// A `peak` above 100 would ask one thread for more than it has, so it is
/// clamped rather than refused: the point of this app is to be easy to run.
fn parse_percent(arg: Option<&str>) -> u8 {
    arg.and_then(|value| value.parse::<u64>().ok())
        .map_or(40, |value| u8::try_from(value.min(100)).unwrap_or(100))
}

fn parse_period(arg: Option<&str>) -> Duration {
    let secs = arg
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30)
        .max(1);
    Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::parse_percent;

    /// The doc comment promises a clamp to 100 for anything above it.
    /// Parsing into `u16` first broke that promise for an argument that
    /// overflows `u16` (65536 and up): the parse itself failed, and the
    /// value silently fell through to the 40 default instead of clamping.
    #[test]
    fn a_value_that_overflows_u16_still_clamps_to_100() {
        assert_eq!(parse_percent(Some("65536")), 100);
        assert_eq!(parse_percent(Some("999999999999")), 100);
    }

    #[test]
    fn an_ordinary_value_passes_through_unclamped() {
        assert_eq!(parse_percent(Some("40")), 40);
    }

    #[test]
    fn a_value_at_or_below_100_is_unaffected() {
        assert_eq!(parse_percent(Some("100")), 100);
        assert_eq!(parse_percent(Some("101")), 100);
    }

    #[test]
    fn a_missing_or_unparsable_argument_defaults_to_40() {
        assert_eq!(parse_percent(None), 40);
        assert_eq!(parse_percent(Some("not-a-number")), 40);
    }
}
