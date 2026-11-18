//! The record of a boot that ended without a stop. A stop leaves its
//! reason in the log, or in the firmware where the log is out of reach
//! ([`crate::stop_record`]). A boot that hangs and is switched off, or that
//! the PC resets, leaves neither, and the next start cannot tell it from
//! one that was shut down: the person sees no text and the log no end.
//!
//! So a boot writes one small firmware variable, `StelaRunning`, just
//! before it asks the firmware to end its boot services: the boot's number
//! in the USB's log, the log's partition, and the time. That is the only
//! write: one per boot, made while the firmware's boot services still run,
//! and none after them, when a variable call can fault in some firmware
//! and would then stop a boot that was going on. The variable is never
//! deleted; the next boot's write replaces it.
//!
//! The next start on the PC reads it ([`Running::decode`]) and decides
//! from the log ([`unfinished`]): the record names the boot before this
//! one in this USB's log, no stop record was carried for it, and its last
//! lines hold none of the lines every stop, restart and power-off writes
//! ([`ENDS`]). Then it says so in the log ([`Unfinished::line`]) and on
//! the screen ([`Unfinished::notice`]), with the last step the log shows
//! running, how long, and whether the step watchdog was on.
//!
//! The step is the last one the log holds. From the firmware's exit until
//! Stela's own driver writes the stick again, lines wait in memory: a boot
//! that ends there shows the step before the exit, and the line says that
//! later lines were not kept.
//!
//! | Offset | Bytes | Field |
//! | --- | --- | --- |
//! | 0 | 8 | magic `STELARUN` |
//! | 8 | 4 | version, 1 |
//! | 12 | 4 | CRC-32 of bytes 16..48 |
//! | 16 | 8 | the boot's number in the USB's log |
//! | 24 | 16 | the GUID of the log's partition |
//! | 40 | 4 | seconds since the PC started, at the write |
//! | 44 | 4 | reserved, zero |
use alloc::{format, string::String};
use stela_contracts::crc::crc32_ieee;

/// The variable's name, under `stop_record::VENDOR`.
pub const NAME: [u16; 13] = {
    let text = b"StelaRunning";
    let mut name = [0; 13];
    let mut at = 0;
    while at < text.len() {
        name[at] = text[at] as u16;
        at += 1;
    }
    name
};
/// `EFI_VARIABLE_NON_VOLATILE | BOOTSERVICE_ACCESS`: kept across a
/// restart; written and read only while boot services run.
pub const ATTRIBUTES: u32 = 0x1 | 0x2;
pub const BYTES: usize = 48;
/// What the variable takes of the firmware's store, with its name and the
/// firmware's own header, and as much again: firmware writes the new value
/// before it frees the old one.
pub const STORE_BYTES: u64 = 2 * (BYTES as u64 + 128);
/// Firmware variable writes a boot makes for this record.
pub const WRITES_PER_BOOT: u32 = 1;
const MAGIC: &[u8; 8] = b"STELARUN";
const VERSION: u32 = 1;

/// A boot that reached the firmware's exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Running {
    /// Its number in the USB's log.
    pub boot: u64,
    /// The GUID of the partition that holds that log.
    pub log: [u8; 16],
    /// Seconds since the PC started when it was written.
    pub seconds: u32,
}

impl Running {
    pub fn encode(&self) -> [u8; BYTES] {
        let mut bytes = [0; BYTES];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.boot.to_le_bytes());
        bytes[24..40].copy_from_slice(&self.log);
        bytes[40..44].copy_from_slice(&self.seconds.to_le_bytes());
        let checksum = crc32_ieee(&bytes[16..]);
        bytes[12..16].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    /// The record in a variable's value; `None` for any other bytes, which
    /// are never read as a boot.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
        if bytes.len() != BYTES
            || &bytes[..8] != MAGIC
            || word(8) != VERSION
            || word(12) != crc32_ieee(&bytes[16..])
            || word(44) != 0
        {
            return None;
        }
        Some(Self {
            boot: u64::from_le_bytes(bytes[16..24].try_into().expect("8 bytes")),
            log: bytes[24..40].try_into().expect("16 bytes"),
            seconds: word(40),
        })
    }
}

/// Whether the firmware's store, with `remaining` bytes free, takes the
/// record and keeps `stop_record::STORE_RESERVE` free after it. Some
/// firmware fails once its store fills.
pub const fn store_takes(remaining: u64) -> bool {
    remaining >= STORE_BYTES + crate::stop_record::STORE_RESERVE
}

/// What a boot's last lines hold when it ended by a stop, a restart or a
/// power-off: one of these is in every such end.
pub const ENDS: [&str; 6] = [
    "STELA FATAL",
    "STELA PANIC",
    "STELA STOP",
    "STELA RESTART",
    "STELA POWER-OFF",
    "POWER powering off",
];
/// The line that starts a boot in the log.
const BOOT_MARKER: &str = "=== STELA BOOT ";
/// The line Stela's own driver writes when it has the stick's log again.
const LOG_RESUMED: &str = "through the USB driver";
/// The most of the log's newest bytes the decision reads.
pub const TAIL_BYTES: usize = 256 << 10;

/// Whether `newest`, the log's newest bytes, answer the question: they
/// hold an end's line or reach back to the start of the last boot, or are
/// all the decision reads.
pub fn enough(newest: &[u8]) -> bool {
    let text = String::from_utf8_lossy(newest);
    newest.len() >= TAIL_BYTES
        || text.contains(BOOT_MARKER)
        || ENDS.iter().any(|end| text.contains(end))
}

/// The step watchdog of the boot that ended, as its lines say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Watchdog {
    On,
    /// The other processors watched the boot processor's steps.
    Processors,
    Unavailable,
    /// No line of it in the lines read.
    Unknown,
}

impl Watchdog {
    pub const fn name(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Processors => "on (the other processors)",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
}

/// A boot that ended without a stop, as the log shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unfinished {
    pub boot: u64,
    /// The last step the log shows running; empty where it shows none.
    pub stage: String,
    /// The step had ended and no other had begun.
    pub after: bool,
    /// The last detail of the step that the log shows ended.
    pub detail: String,
    pub watchdog: Watchdog,
    /// Seconds from the step's start to the log's last line.
    pub seconds: Option<u64>,
    /// The log holds no line from after the firmware's exit: the boot went
    /// on from the stage for a time the log does not show.
    pub unkept: bool,
}

/// The milliseconds a log line's stamp gives, `[  12.345] `, and the rest.
fn stamped(line: &str) -> (Option<u64>, &str) {
    let parse = || {
        let inner = line.strip_prefix('[')?;
        let (stamp, rest) = inner.split_once("] ")?;
        let (seconds, millis) = stamp.trim().split_once('.')?;
        let at = seconds.parse::<u64>().ok()? * 1000 + millis.parse::<u64>().ok()?;
        Some((at, rest))
    };
    match parse() {
        Some((at, rest)) => (Some(at), rest),
        None => (None, line),
    }
}

/// Decide whether the boot `running` names ended without a stop, at the
/// start of boot `this_boot` of the log on partition `this_log`:
/// `carried` is the boot of a stop record this start copied from the
/// firmware, and `newest` the log's newest lines from before this boot's
/// own ([`enough`]). `None` where the record is another USB's or an older
/// boot's, which an earlier start told, or where the boot's end is in the
/// log or the firmware.
pub fn unfinished(
    running: &Running,
    this_boot: u64,
    this_log: &[u8; 16],
    carried: Option<u64>,
    newest: &[u8],
) -> Option<Unfinished> {
    if running.log != *this_log
        || running.boot.checked_add(1) != Some(this_boot)
        || carried == Some(running.boot)
    {
        return None;
    }
    let text = String::from_utf8_lossy(newest);
    // The last boot's lines: all that was read where its start is older.
    let (whole, lines) = match text.rfind(BOOT_MARKER) {
        Some(at) => (true, &text[at..]),
        None => (false, &text[..]),
    };
    if ENDS.iter().any(|end| lines.contains(end)) {
        return None;
    }
    let mut found = Unfinished {
        boot: running.boot,
        stage: String::new(),
        after: false,
        detail: String::new(),
        watchdog: Watchdog::Unknown,
        seconds: None,
        unkept: whole && !lines.contains(LOG_RESUMED),
    };
    let (mut began, mut last) = (None, None);
    for line in lines.lines() {
        let (at, line) = stamped(line);
        last = at.or(last);
        let Some(step) = line.strip_prefix("STEP ") else {
            continue;
        };
        if let Some(watchdog) = step.strip_prefix("WATCHDOG ") {
            found.watchdog = if watchdog.starts_with("on") {
                Watchdog::On
            } else if watchdog.starts_with("the other processors") {
                Watchdog::Processors
            } else {
                Watchdog::Unavailable
            };
        } else if let Some(detail) = step.strip_prefix("DETAIL ") {
            let detail = detail
                .rsplit_once(" ms=")
                .map_or(detail, |(detail, _)| detail);
            found.detail = String::from(detail);
        } else if let Some((title, _)) = step.rsplit_once(" t=").filter(|(_, time)| {
            time.bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
        }) {
            // `STEP <title> t=<seconds>`: a step begins.
            found.stage = String::from(title);
            (found.after, found.detail) = (false, String::new());
            began = at;
        } else if step.contains(" ms=") && !found.stage.is_empty() {
            // `STEP <mark> <title> … ms=<n>`: the running step ended.
            found.after = true;
        }
    }
    found.seconds = began
        .zip(last)
        .map(|(began, last)| last.saturating_sub(began) / 1000);
    Some(found)
}

impl Unfinished {
    /// The stage as a person reads it.
    fn stage_text(&self) -> String {
        let mut text = match (self.stage.is_empty(), self.after) {
            (true, _) => String::from("not in the log's last lines"),
            (false, false) => format!("\"{}\"", self.stage),
            (false, true) => format!("after \"{}\"", self.stage),
        };
        if !self.detail.is_empty() {
            text.push_str(&format!(", past \"{}\"", self.detail));
        }
        text
    }

    /// `PREVIOUS BOOT ended without a stop (boot N): last stage …,
    /// watchdog=…, N s in the stage`.
    pub fn line(&self) -> String {
        let seconds = match self.seconds {
            Some(seconds) => format!("{seconds} s in the stage by the log's last line"),
            None => String::from("time in the stage unknown"),
        };
        let unkept = match self.unkept {
            true => {
                "; the log holds no line from after the firmware's exit, so the boot may have \
                 gone on to a later stage"
            }
            false => "",
        };
        format!(
            "PREVIOUS BOOT ended without a stop (boot {}): last stage {}, watchdog={}, \
             {seconds}{unkept}\n",
            self.boot,
            self.stage_text(),
            self.watchdog.name()
        )
    }

    /// What the screen says: short, and where the log is.
    pub fn notice(&self) -> String {
        let stage = match (self.stage.is_empty(), self.unkept) {
            (true, _) => String::new(),
            (false, false) => format!(" during \"{}\"", self.stage),
            (false, true) => format!(" during \"{}\" or soon after", self.stage),
        };
        format!(
            "The last start of Stela on this PC did not end by itself: the PC was switched off \
             or reset{stage}. If Stela had stopped working, its log is on this USB: on any \
             computer with stela, plug in this USB and run `stela log`, which saves it to \
             log.txt; include that file in a report."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: [u8; 16] = [7; 16];

    fn running(boot: u64) -> Running {
        Running {
            boot,
            log: LOG,
            seconds: 41,
        }
    }

    #[test]
    fn the_record_is_read_back_and_other_bytes_are_never_a_boot() {
        let record = running(12);
        let bytes = record.encode();
        assert_eq!(bytes.len(), BYTES);
        assert_eq!(Running::decode(&bytes), Some(record));
        for at in [0, 8, 13, 16, 30, 41, 44] {
            let mut damaged = bytes;
            damaged[at] ^= 1;
            assert_eq!(Running::decode(&damaged), None, "{at}");
        }
        assert_eq!(Running::decode(&bytes[..BYTES - 1]), None);
        assert_eq!(Running::decode(&[0; BYTES]), None);
        assert_eq!(Running::decode(&[]), None);
        assert_eq!(
            NAME.iter()
                .map(|&unit| unit as u8 as char)
                .collect::<String>(),
            "StelaRunning\0"
        );
        // Linux's reserve stays free after both copies of the variable.
        assert!(!store_takes(5120 + 351));
        assert!(store_takes(5120 + 352));
        assert_eq!(WRITES_PER_BOOT, 1);
    }

    /// A boot switched off while it hung in a step after the firmware's
    /// exit: the log's lines end before the exit.
    const HUNG_AT_EXIT: &str = "[   20.100] STEP o Reading Stela's settings ms=12\n\
        === STELA BOOT 12 ===\n\
        [    4.000] HOST version=0.5.0 cpu=\"test\"\n\
        [   30.250] STEP Preparing the Mac's memory t=30.250\n\
        [   30.900] STEP o Preparing the Mac's memory ms=650\n\
        [   31.000] NATIVE MMIO memory map before exit: sealed\n\
        [   31.004] STEP Taking control of the other processors t=31.004\n";

    #[test]
    fn a_boot_that_ended_at_the_firmware_s_exit_is_told_with_its_last_kept_stage() {
        let found = unfinished(&running(12), 13, &LOG, None, HUNG_AT_EXIT.as_bytes()).unwrap();
        assert_eq!(
            found,
            Unfinished {
                boot: 12,
                stage: String::from("Taking control of the other processors"),
                after: false,
                detail: String::new(),
                watchdog: Watchdog::Unknown,
                seconds: Some(0),
                unkept: true,
            }
        );
        assert_eq!(
            found.line(),
            "PREVIOUS BOOT ended without a stop (boot 12): last stage \"Taking control of the \
             other processors\", watchdog=unknown, 0 s in the stage by the log's last line; the \
             log holds no line from after the firmware's exit, so the boot may have gone on to \
             a later stage\n"
        );
        let notice = found.notice();
        assert!(notice.contains(
            "switched off or reset during \"Taking control of the other processors\" or soon \
             after"
        ));
        assert!(notice.contains("on any computer with stela"), "{notice}");
        assert!(notice.contains("`stela log`"), "{notice}");
    }

    #[test]
    fn a_boot_that_hung_with_the_log_open_names_its_step_its_time_and_the_watchdog() {
        let log = format!(
            "{HUNG_AT_EXIT}\
             [   31.900] STEP WATCHDOG on: a Linux step that does not return within 30 s stops \
             the boot (AMD core counter 0)\n\
             [   33.000] BOOT LOG FILE \\EFI\\stela\\boot.log through the USB driver boot=12 \
             head=0x8000\n\
             [   33.500] STEP o Taking control of the other processors ms=2496\n\
             [   33.600] STEP Starting the GPU t=33.600\n\
             [   35.000] STEP DETAIL loading its firmware ms=1400\n\
             [   95.250] LINUX <6> nouveau 0000:01:00.0: still waiting\n"
        );
        let found = unfinished(&running(12), 13, &LOG, None, log.as_bytes()).unwrap();
        assert_eq!(found.stage, "Starting the GPU");
        assert_eq!(found.detail, "loading its firmware");
        assert_eq!(
            (found.seconds, found.unkept, found.after),
            (Some(61), false, false)
        );
        assert_eq!(found.watchdog, Watchdog::On);
        assert_eq!(
            found.line(),
            "PREVIOUS BOOT ended without a stop (boot 12): last stage \"Starting the GPU\", \
             past \"loading its firmware\", watchdog=on, 61 s in the stage by the log's last \
             line\n"
        );
        assert!(
            found
                .notice()
                .contains("reset during \"Starting the GPU\". If")
        );

        // Between two steps, and the other watchdog lines.
        let between = format!(
            "{log}[   96.000] STEP WATCHDOG the other processors watch the boot processor's \
             Linux steps\n[   97.000] STEP o Starting the GPU (started) ms=63400\n"
        );
        let found = unfinished(&running(12), 13, &LOG, None, between.as_bytes()).unwrap();
        assert!(found.after);
        assert_eq!(found.watchdog, Watchdog::Processors);
        assert!(
            found
                .line()
                .contains("last stage after \"Starting the GPU\"")
        );
        let unavailable = "=== STELA BOOT 12 ===\nSTEP WATCHDOG unavailable: no counter\n";
        let found = unfinished(&running(12), 13, &LOG, None, unavailable.as_bytes()).unwrap();
        assert_eq!(found.watchdog, Watchdog::Unavailable);
        assert_eq!(found.seconds, None);
        assert!(
            found
                .line()
                .contains("last stage not in the log's last lines")
        );
        assert!(found.line().contains("time in the stage unknown"));
    }

    #[test]
    fn a_boot_whose_end_is_recorded_or_that_is_not_the_last_is_not_told() {
        let tail = HUNG_AT_EXIT.as_bytes();
        assert!(unfinished(&running(12), 13, &LOG, None, tail).is_some());
        // A stop record carried from the firmware for that boot.
        assert_eq!(unfinished(&running(12), 13, &LOG, Some(12), tail), None);
        assert!(unfinished(&running(12), 13, &LOG, Some(9), tail).is_some());
        // Another USB's record, an older boot's, and a log that started over.
        assert_eq!(unfinished(&running(12), 13, &[8; 16], None, tail), None);
        assert_eq!(unfinished(&running(12), 14, &LOG, None, tail), None);
        assert_eq!(unfinished(&running(12), 1, &LOG, None, tail), None);
        assert_eq!(unfinished(&running(u64::MAX), 0, &LOG, None, tail), None);
        // Every end's line in the boot's last lines.
        for end in [
            "[  900.000] STELA FATAL the GPU stopped answering\n",
            "STELA PANIC at src/main.rs\n",
            "[  900.000] STELA STOPPED code=0x7\n",
            "[  900.000] STELA RESTART code=0x5\n",
            "[  900.000] STELA POWER-OFF code=0x6\n",
            "[  900.000] POWER powering off through the firmware\n",
        ] {
            let ended = format!("{HUNG_AT_EXIT}{end}");
            assert_eq!(
                unfinished(&running(12), 13, &LOG, None, ended.as_bytes()),
                None,
                "{end}"
            );
            assert!(enough(end.as_bytes()), "{end}");
        }
        // An end in an earlier boot is not this boot's.
        let earlier = format!("[  900.000] STELA RESTART code=0x5\n{HUNG_AT_EXIT}");
        assert!(unfinished(&running(12), 13, &LOG, None, earlier.as_bytes()).is_some());
        // A long boot: its start is older than the lines read.
        let long = "[ 7200.000] BEAT SHORT blocks=9\n[ 7210.000] BEAT SHORT blocks=10\n";
        let found = unfinished(&running(12), 13, &LOG, None, long.as_bytes()).unwrap();
        assert!(!found.unkept && found.stage.is_empty());
        assert!(!enough(long.as_bytes()));
        assert!(enough(HUNG_AT_EXIT.as_bytes()));
        assert!(enough(&[b'x'; TAIL_BYTES]));
    }
}
