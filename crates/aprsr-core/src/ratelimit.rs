//! Per-client submission rate limiting.
//!
//! A token bucket: a client may send at a sustained rate, and may burst above it for as long
//! as it has saved up credit. That shape is chosen deliberately over a fixed window, because
//! APRS traffic is bursty in a way that is entirely legitimate — an IGate that has been quiet
//! all night gates six packets in one second when a net starts, and a fixed window would
//! either refuse that or have to be set so loose it stopped limiting anything.
//!
//! Time is a parameter, never read from a clock. That is the same rule `dupecheck` follows
//! and for the same reason: a limiter that read the clock could not be tested for what it
//! does at a boundary, and boundaries are the only interesting part.
//!
//! ## What this is and is not for
//!
//! It bounds one misbehaving or misconfigured client — a station beaconing every second, a
//! script in a loop — so it cannot fill the dispatch queue at everybody else's expense. It is
//! not a defence against a distributed flood, which arrives on a thousand connections that
//! are each within their limit; that is a job for the address list in [`crate::access`] and
//! for the firewall in front of the server.

/// Tokens are counted in thousandths so a fractional refill rate needs no floating point.
///
/// A rate of 1.5 packets per second is a perfectly reasonable thing to configure, and doing
/// this in `f64` would put rounding behaviour into a security-adjacent limit. Integers make
/// the arithmetic exact and the tests exact with it.
const SCALE: u64 = 1_000;

/// A token bucket over one client's submissions.
///
/// One of these lives beside each connection. It is deliberately small — three integers —
/// because a busy server holds thousands.
#[derive(Debug, Clone, Copy)]
pub struct RateLimiter {
    /// Sustained rate, in thousandths of a packet per second.
    refill_milli: u64,
    /// Burst allowance, in thousandths of a packet.
    capacity_milli: u64,
    /// Credit available now, in thousandths of a packet.
    tokens_milli: u64,
    /// When the credit was last brought up to date.
    last: u64,
}

impl RateLimiter {
    /// A limiter allowing `per_second` sustained, with `burst` packets of saved credit.
    ///
    /// Starts full: a client that has just connected has not used anything, and making it
    /// wait for its first packet would penalise the well-behaved case to no purpose.
    ///
    /// A `burst` of zero is raised to one. A bucket that cannot hold a single token is a
    /// client that can never send anything, and an operator writing `burst = 0` means "no
    /// allowance beyond the sustained rate" far more often than they mean "mute this port".
    /// Same reasoning as `per_second = 0` meaning unlimited: a number in a configuration
    /// file should not be able to silently disable the thing it configures.
    #[must_use]
    pub const fn new(per_second: u32, burst: u32, now: u64) -> Self {
        let capacity_milli = if burst == 0 { 1 } else { burst as u64 }.saturating_mul(SCALE);
        Self {
            refill_milli: (per_second as u64).saturating_mul(SCALE),
            capacity_milli,
            tokens_milli: capacity_milli,
            last: now,
        }
    }

    /// Whether this limiter enforces anything.
    ///
    /// A rate of zero means unlimited rather than "refuse everything" — the latter would
    /// make an operator who set the option to zero silently mute their own server, and there
    /// are far clearer ways to say that.
    #[must_use]
    pub const fn is_unlimited(&self) -> bool {
        self.refill_milli == 0
    }

    /// Credit available now, in whole packets. For the dashboard and for tests.
    #[must_use]
    pub const fn available(&self) -> u64 {
        self.tokens_milli / SCALE
    }

    /// Take one token if there is one. Returns false when the client is over its rate.
    ///
    /// `now` must be Unix seconds. A `now` that moves backwards — a clock adjustment, an NTP
    /// step — refills nothing and is otherwise ignored, so it can never grant credit or
    /// reset the bucket. Failing closed for a second beats a limiter that can be bypassed by
    /// waiting for a clock correction.
    pub fn try_take(&mut self, now: u64) -> bool {
        if self.is_unlimited() {
            return true;
        }

        let elapsed = now.saturating_sub(self.last);
        if elapsed > 0 {
            self.tokens_milli = self
                .tokens_milli
                .saturating_add(elapsed.saturating_mul(self.refill_milli))
                .min(self.capacity_milli);
            self.last = now;
        }

        if self.tokens_milli >= SCALE {
            self.tokens_milli -= SCALE;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn a_fresh_limiter_starts_with_its_full_burst() {
        let limiter = RateLimiter::new(10, 20, 1_000);
        assert_eq!(limiter.available(), 20);
        assert!(!limiter.is_unlimited());
    }

    /// The burst is the whole point: an IGate quiet all night gates six packets the moment a
    /// net starts, and that is not abuse.
    #[test]
    fn a_burst_is_allowed_up_to_the_capacity() {
        let mut limiter = RateLimiter::new(1, 5, 1_000);
        for i in 0..5 {
            assert!(
                limiter.try_take(1_000),
                "packet {i} of the burst was refused"
            );
        }
        assert!(!limiter.try_take(1_000), "the sixth exceeded the burst");
    }

    #[test]
    fn credit_refills_at_the_configured_rate() {
        let mut limiter = RateLimiter::new(2, 4, 1_000);
        for _ in 0..4 {
            assert!(limiter.try_take(1_000));
        }
        assert!(!limiter.try_take(1_000));

        // One second later, two packets' worth of credit.
        assert!(limiter.try_take(1_001));
        assert!(limiter.try_take(1_001));
        assert!(!limiter.try_take(1_001));
    }

    #[test]
    fn credit_does_not_accumulate_past_the_burst() {
        let mut limiter = RateLimiter::new(10, 3, 1_000);
        // An hour of silence still only buys the burst.
        assert!(limiter.try_take(4_600));
        assert_eq!(limiter.available(), 2);
        assert!(limiter.try_take(4_600));
        assert!(limiter.try_take(4_600));
        assert!(!limiter.try_take(4_600));
    }

    /// Zero means unlimited. Refusing everything would let an operator mute their own server
    /// by setting a number, which is not what any number in a configuration file should do.
    #[test]
    fn a_rate_of_zero_means_unlimited() {
        let mut limiter = RateLimiter::new(0, 0, 1_000);
        assert!(limiter.is_unlimited());
        for _ in 0..10_000 {
            assert!(limiter.try_take(1_000));
        }
    }

    /// A limiter that could be reset by a clock going backwards would be bypassable by
    /// anybody who could wait for an NTP correction.
    #[test]
    fn a_clock_that_moves_backwards_grants_nothing() {
        let mut limiter = RateLimiter::new(1, 2, 1_000);
        assert!(limiter.try_take(1_000));
        assert!(limiter.try_take(1_000));
        assert!(!limiter.try_take(1_000));

        assert!(!limiter.try_take(900), "the clock went back, not forward");
        assert!(!limiter.try_take(0));
        // ...and the original schedule still applies once it moves forward again.
        assert!(limiter.try_take(1_001));
    }

    /// Over time, a client that hammers the limiter gets exactly its sustained rate.
    ///
    /// The burst is set to the rate so that a whole second's credit can be taken at once,
    /// which is what makes the total come out as rate × seconds rather than being shaped by
    /// the bucket depth.
    #[rstest]
    #[case(1, 1, 1)] // one per second, one second
    #[case(3, 1, 3)] // three per second
    #[case(1, 4, 4)] // one per second, over four seconds
    #[case(5, 4, 20)]
    fn sustained_rate_over_time(
        #[case] per_second: u32,
        #[case] seconds: u64,
        #[case] expected: usize,
    ) {
        // Starts empty rather than full, so the count is what was earned rather than what
        // was earned plus the opening credit.
        let mut limiter = RateLimiter::new(per_second, per_second, 0);
        for _ in 0..per_second {
            assert!(limiter.try_take(0), "draining the opening credit");
        }

        let mut allowed = 0;
        for second in 1..=seconds {
            for _ in 0..100 {
                if limiter.try_take(second) {
                    allowed += 1;
                }
            }
        }
        assert_eq!(allowed, expected);
    }

    /// A burst of zero must not mute the client — it means "no allowance beyond the rate".
    #[test]
    fn a_burst_of_zero_still_lets_the_sustained_rate_through() {
        let mut limiter = RateLimiter::new(1, 0, 0);
        assert!(limiter.try_take(0), "the opening token");
        assert!(!limiter.try_take(0));
        assert!(limiter.try_take(1), "a second later, one more");
        assert!(!limiter.try_take(1));
    }

    proptest::proptest! {
        /// The arithmetic runs against a clock the server does not control.
        #[test]
        fn never_panics_on_any_clock(
            per_second in 0u32..1_000,
            burst in 0u32..1_000,
            start in 0u64..u64::MAX,
            step in 0u64..u64::MAX,
        ) {
            let mut limiter = RateLimiter::new(per_second, burst, start);
            let _ = limiter.try_take(start);
            let _ = limiter.try_take(start.saturating_add(step));
            let _ = limiter.try_take(start.saturating_sub(step));
        }

        /// A limited client can never exceed burst + rate × elapsed, whatever it does.
        #[test]
        fn a_limited_client_cannot_exceed_its_allowance(
            per_second in 1u32..20,
            burst in 0u32..20,
            seconds in 0u64..30,
        ) {
            let mut limiter = RateLimiter::new(per_second, burst, 0);
            let mut allowed = 0u64;
            // Hammer it every second, far harder than the limit.
            for second in 0..=seconds {
                for _ in 0..100 {
                    if limiter.try_take(second) {
                        allowed += 1;
                    }
                }
            }
            let ceiling = u64::from(burst.max(1)) + u64::from(per_second) * seconds;
            proptest::prop_assert!(
                allowed <= ceiling,
                "allowed {allowed} with a ceiling of {ceiling}"
            );
        }
    }
}
