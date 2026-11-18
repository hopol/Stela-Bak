//! The clock each PE's host core delivered and what held it back, for the
//! heartbeat's `PES CLOCK` line. Guest code runs at 180-440 million guest
//! instructions per second of guest code on reporters' laptops against 760
//! on a desktop whose boot-time clock is only 1.1-1.3 times theirs, and
//! some laptops measure 1.6-3.0 GHz at boot against a 4.1 GHz turbo: a
//! report should say the clock each core ran at under load, and whether a
//! thermal or power limit held it there.
//!
//! - IA32_MPERF (E7H) counts at the TSC's rate and IA32_APERF (E8H) at the
//!   core's actual clock, both only while the core runs (C0), where
//!   CPUID.06H:ECX[0] enumerates them (Intel SDM Vol. 3B 15.2; AMD APM Vol. 2
//!   17.3): their ratio over an interval scales the TSC rate to the clock
//!   delivered, as Linux's `aperfmperf_get_khz` does, and MPERF over the
//!   interval's TSC ticks is the share of it the core ran.
//! - IA32_THERM_STATUS (19CH), where CPUID.06H:EAX[0] enumerates the digital
//!   thermal sensor (Intel SDM Vol. 3B 15.8.5.2): bit 0 the core is over its
//!   thermal limit now, bit 2 PROCHOT# or FORCEPR# slows it now, bit 10 a
//!   power limit holds it below the requested performance now (where
//!   CPUID.06H:EAX[4] enumerates it), bits 22:16 its degrees below the
//!   highest allowed temperature, valid with bit 31. Only the current bits
//!   are read: the sticky log bits keep whatever firmware left.
//!
//! A sample reads two or three registers on the core itself, at most once
//! per [`SAMPLE_TICKS`] of its TSC (about 60 times a second at 4 GHz); the
//! check between costs one TSC read where the PE publishes its counts.
use crate::msr::{Bit, Gate, Msr, Register};
use alloc::{format, string::String};
use stela_engine::machine::engine::HostClock;

const APERF_MPERF: Bit = Bit::new(6, Register::Ecx, 0);
const THERMAL_SENSOR: Bit = Bit::new(6, Register::Eax, 0);
const POWER_LIMIT_NOTICE: Bit = Bit::new(6, Register::Eax, 4);

pub const MPERF: Msr = Msr::new(0xe7, Gate::Cpuid(APERF_MPERF));
pub const APERF: Msr = Msr::new(0xe8, Gate::Cpuid(APERF_MPERF));
pub const THERM_STATUS: Msr = Msr::new(0x19c, Gate::Cpuid(THERMAL_SENSOR));

const HOT: u64 = 1 << 0;
const PROCHOT: u64 = 1 << 2;
const POWER_LIMITED: u64 = 1 << 10;
const READING_VALID: u64 = 1 << 31;
const READING_SHIFT: u32 = 16;
const READING_MASK: u64 = 0x7f;

/// TSC ticks between two samples of a core: 2^26, 13-34 ms from 5 GHz to
/// 2 GHz, so a 10 s beat holds hundreds of samples and a busy core reads its
/// registers no more than about 60 times a second.
pub const SAMPLE_TICKS: u64 = 1 << 26;

/// Which of the registers this core has, found once: CPUID exits to a
/// hypervisor, so it is never asked per sample.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Features {
    pub counters: bool,
    pub thermal: bool,
    pub power: bool,
}

impl Features {
    /// This processor's enumeration.
    pub fn read() -> Self {
        Self {
            counters: APERF.implemented(),
            thermal: THERM_STATUS.implemented(),
            power: POWER_LIMIT_NOTICE.present(),
        }
    }
}

/// One reading of the registers [`Features`] names; zero where it has none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    pub actual: u64,
    pub reference: u64,
    pub therm_status: u64,
}

impl Reading {
    /// The registers `features` names, on this processor.
    ///
    /// # Safety
    /// CPL0 on the processor whose enumeration `features` is.
    pub unsafe fn take(features: Features) -> Self {
        let mut reading = Self::default();
        // SAFETY: the caller's contract; `features` holds only registers
        // CPUID enumerates on this processor, and reading them has no
        // effect.
        unsafe {
            if features.counters {
                reading.reference = MPERF.read();
                reading.actual = APERF.read();
            }
            if features.thermal {
                reading.therm_status = THERM_STATUS.read();
            }
        }
        reading
    }
}

/// One core's samples so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    features: Features,
    /// The TSC at the last sample, none before the first.
    last: Option<u64>,
    totals: HostClock,
}

impl Clock {
    pub fn new(features: Features) -> Self {
        Self {
            features,
            last: None,
            totals: HostClock {
                counters: features.counters,
                thermal: features.thermal,
                ..HostClock::default()
            },
        }
    }

    /// The totals at TSC `now`, after a sample through `read` when the last
    /// is [`SAMPLE_TICKS`] old or there is none. A core without either
    /// register set is never read.
    pub fn sample(&mut self, now: u64, read: impl FnOnce(Features) -> Reading) -> HostClock {
        let due = self
            .last
            .is_none_or(|last| now.wrapping_sub(last) >= SAMPLE_TICKS);
        if !due || !(self.features.counters || self.features.thermal) {
            return self.totals;
        }
        self.last = Some(now);
        let reading = read(self.features);
        let totals = &mut self.totals;
        if self.features.counters {
            totals.actual = reading.actual;
            totals.reference = reading.reference;
        }
        if self.features.thermal {
            let status = reading.therm_status;
            totals.samples += 1;
            totals.hot += u64::from(status & HOT != 0);
            totals.prochot += u64::from(status & PROCHOT != 0);
            totals.power += u64::from(self.features.power && status & POWER_LIMITED != 0);
            totals.headroom = (status & READING_VALID != 0)
                .then_some(((status >> READING_SHIFT) & READING_MASK) as u8);
        }
        *totals
    }
}

/// One PE's interval: its core's counts at the beat before and now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeClock {
    pub before: HostClock,
    pub now: HostClock,
}

impl PeClock {
    /// The clock delivered while running, in MHz, and the running share of
    /// `interval` TSC ticks in percent; none without the counters, or a
    /// clock of none where the core never ran.
    fn delivered(&self, tsc_hz: u64, interval: u64) -> Option<(Option<u64>, u64)> {
        if !self.now.counters {
            return None;
        }
        let actual = self.now.actual.wrapping_sub(self.before.actual);
        let reference = self.now.reference.wrapping_sub(self.before.reference);
        let mhz = (reference != 0).then(|| {
            (u128::from(tsc_hz) * u128::from(actual) / u128::from(reference) / 1_000_000) as u64
        });
        let running = (u128::from(reference) * 100 / u128::from(interval.max(1))).min(100) as u64;
        Some((mhz, running))
    }

    /// Its thermal samples' shares in percent (over the limit, power
    /// limited, PROCHOT) and the last reading's headroom; none without the
    /// sensor or a sample.
    fn limits(&self) -> Option<([u64; 3], Option<u8>)> {
        let samples = self.now.samples.wrapping_sub(self.before.samples);
        if !self.now.thermal || samples == 0 {
            return None;
        }
        let share = |now: u64, before: u64| now.wrapping_sub(before) * 100 / samples;
        Some((
            [
                share(self.now.hot, self.before.hot),
                share(self.now.power, self.before.power),
                share(self.now.prochot, self.before.prochot),
            ],
            self.now.headroom,
        ))
    }
}

/// `PES CLOCK tsc_mhz=3793 mhz/run%[/hot%/power%/prochot%/below_max_c]
/// 0:4310/100 1:3890/100/0/37/0/22 … all:4012/97`: per PE the clock its core
/// delivered while it ran over the `interval` of TSC ticks at `tsc_hz` and
/// the share it ran, then where the core has a thermal sensor the share of
/// samples over its thermal limit, held by a power limit and slowed by
/// PROCHOT, and its last reading's degrees below the highest allowed
/// temperature; `-` for what the core cannot tell. `all` is the clock
/// weighted by each core's running time and the mean running share.
pub fn pes_clock(tsc_hz: u64, interval: u64, pes: &[PeClock]) -> String {
    let mut line = format!(
        "PES CLOCK tsc_mhz={} mhz/run%[/hot%/power%/prochot%/below_max_c]",
        tsc_hz / 1_000_000
    );
    let (mut actual, mut reference, mut counted) = (0u128, 0u128, 0u64);
    for (pe, clock) in pes.iter().enumerate() {
        line.push_str(&format!(" {pe}:"));
        match clock.delivered(tsc_hz, interval) {
            Some((mhz, running)) => {
                match mhz {
                    Some(mhz) => line.push_str(&format!("{mhz}/{running}")),
                    None => line.push_str(&format!("-/{running}")),
                }
                actual += u128::from(clock.now.actual.wrapping_sub(clock.before.actual));
                reference += u128::from(clock.now.reference.wrapping_sub(clock.before.reference));
                counted += 1;
            }
            None => line.push('-'),
        }
        if let Some(([hot, power, prochot], headroom)) = clock.limits() {
            line.push_str(&format!("/{hot}/{power}/{prochot}/"));
            match headroom {
                Some(degrees) => line.push_str(&format!("{degrees}")),
                None => line.push('-'),
            }
        }
    }
    if counted == 0 || reference == 0 {
        line.push_str(" all:-");
    } else {
        let mhz = u128::from(tsc_hz) * actual / reference / 1_000_000;
        let running = reference * 100 / (u128::from(interval.max(1)) * u128::from(counted));
        line.push_str(&format!(" all:{mhz}/{}", running.min(100)));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Features = Features {
        counters: true,
        thermal: true,
        power: true,
    };

    fn status(bits: u64, below: u64) -> u64 {
        bits | READING_VALID | below << READING_SHIFT
    }

    #[test]
    fn a_core_is_read_at_most_once_per_sample_interval() {
        let mut clock = Clock::new(ALL);
        let mut reads = 0;
        let read = |actual| {
            move |_: Features| Reading {
                actual,
                reference: actual / 2,
                therm_status: status(0, 40),
            }
        };
        for now in [1000, 1001, 1000 + SAMPLE_TICKS - 1] {
            let totals = clock.sample(now, |features| {
                reads += 1;
                read(10)(features)
            });
            assert_eq!(totals.actual, 10);
        }
        assert_eq!(reads, 1);
        let totals = clock.sample(1000 + SAMPLE_TICKS, read(30));
        assert_eq!(
            (totals.actual, totals.reference, totals.samples),
            (30, 15, 2)
        );
        // The TSC wraps between two samples.
        let mut clock = Clock::new(ALL);
        clock.sample(u64::MAX - 10, read(1));
        assert_eq!(clock.sample(SAMPLE_TICKS, read(2)).actual, 2);
    }

    #[test]
    fn a_core_without_the_registers_is_never_read() {
        let mut clock = Clock::new(Features::default());
        let totals = clock.sample(0, |_| unreachable!("no register to read"));
        assert_eq!(totals, HostClock::default());
        // Counters without a sensor: no thermal sample counts.
        let mut clock = Clock::new(Features {
            counters: true,
            ..Features::default()
        });
        let totals = clock.sample(0, |_| Reading {
            actual: 5,
            reference: 4,
            therm_status: status(HOT, 0),
        });
        assert_eq!((totals.actual, totals.samples, totals.hot), (5, 0, 0));
        assert!(totals.counters && !totals.thermal);
    }

    #[test]
    fn thermal_samples_count_each_current_limit() {
        let mut clock = Clock::new(ALL);
        let samples = [
            status(0, 30),
            status(HOT | PROCHOT, 0),
            status(POWER_LIMITED, 12),
            status(POWER_LIMITED, 9),
            // Log bits (1, 3, 11) are history, not the current state.
            status(0b1010 | 1 << 11, 20),
        ];
        let mut totals = HostClock::default();
        for (index, therm_status) in samples.into_iter().enumerate() {
            totals = clock.sample(index as u64 * SAMPLE_TICKS, |_| Reading {
                therm_status,
                ..Reading::default()
            });
        }
        assert_eq!(
            (totals.samples, totals.hot, totals.prochot, totals.power),
            (5, 1, 1, 2)
        );
        assert_eq!(totals.headroom, Some(20));
        // A reading not marked valid gives no headroom; without the power
        // notification the power bit is not read.
        let mut clock = Clock::new(Features {
            power: false,
            ..ALL
        });
        let totals = clock.sample(0, |_| Reading {
            therm_status: POWER_LIMITED | 25 << READING_SHIFT,
            ..Reading::default()
        });
        assert_eq!((totals.power, totals.headroom), (0, None));
    }

    #[test]
    fn the_line_states_each_cores_clock_running_share_and_limits() {
        let tsc_hz = 3_800_000_000;
        let interval = 38_000_000_000;
        let counted = |actual: u64, reference: u64| HostClock {
            actual,
            reference,
            counters: true,
            ..HostClock::default()
        };
        let pes = [
            // Ran the whole interval at 1.25 times the TSC's rate.
            PeClock {
                before: counted(100, 100),
                now: counted(100 + 47_500_000_000, 100 + interval),
            },
            // Ran half of it at half the TSC's rate, held by a power limit
            // in 3 of 4 samples, 15 degrees below its highest.
            PeClock {
                before: HostClock {
                    thermal: true,
                    samples: 10,
                    power: 2,
                    ..counted(0, 0)
                },
                now: HostClock {
                    thermal: true,
                    samples: 14,
                    power: 5,
                    hot: 1,
                    headroom: Some(15),
                    ..counted(interval / 4, interval / 2)
                },
            },
            // Never ran, and a core with no counters.
            PeClock {
                before: counted(7, 7),
                now: counted(7, 7),
            },
            PeClock::default(),
        ];
        assert_eq!(
            pes_clock(tsc_hz, interval, &pes),
            "PES CLOCK tsc_mhz=3800 mhz/run%[/hot%/power%/prochot%/below_max_c] 0:4750/100 \
             1:1900/50/25/75/0/15 2:-/0 3:- all:3800/50"
        );
        assert_eq!(
            pes_clock(tsc_hz, interval, &[PeClock::default()]),
            "PES CLOCK tsc_mhz=3800 mhz/run%[/hot%/power%/prochot%/below_max_c] 0:- all:-"
        );
    }
}
