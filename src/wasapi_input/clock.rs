use crate::RATE;

const FALLBACK_LIMIT: i64 = (RATE / 4) as i64;
const FUTURE_LIMIT: i64 = (RATE / 50) as i64;
const GAP_LIMIT: i64 = (RATE / 500) as i64;

#[derive(Default)]
pub(super) struct PacketClock {
    next_frame: u64,
    next_stamp: Option<i64>,
}

pub(super) struct Stamp {
    pub start: i64,
    pub position: u64,
    pub reset: bool,
    pub measured: bool,
}

impl PacketClock {
    pub(super) fn stamp(
        &mut self,
        len: usize,
        arrival: i64,
        measured: Option<i64>,
        broken: bool,
    ) -> Stamp {
        let measured = measured.filter(|&stamp| stamp <= arrival + FUTURE_LIMIT);
        let estimate = arrival - len as i64;
        let expected = self.next_stamp.filter(|_| !broken);
        let (start, reset) = if let Some(stamp) = measured {
            // WASAPI's position can be in endpoint frames while PCM arrives
            // at the requested client rate. Detect gaps on the common clock.
            let reset = expected.is_none_or(|end| (stamp - end).abs() > GAP_LIMIT);
            (stamp, reset)
        } else if let Some(expected) =
            expected.filter(|&next| (next - estimate).abs() <= FALLBACK_LIMIT)
        {
            (expected, false)
        } else {
            (estimate, true)
        };
        let position = self.next_frame;
        self.next_frame += len as u64;
        self.next_stamp = Some(start + len as i64);
        Stamp {
            start,
            position,
            reset,
            measured: measured.is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_flags_or_bogus_future_timestamp_do_not_poison_position() {
        let mut clock = PacketClock::default();
        assert!(clock.stamp(480, 480, Some(0), false).reset);
        let estimated = clock.stamp(480, 1000, Some(1_000_000), false);
        assert!(!estimated.measured);
        assert!(!estimated.reset);
        assert_eq!((estimated.start, estimated.position), (480, 480));
        let measured = clock.stamp(480, 1440, Some(960), false);
        assert!(measured.measured && !measured.reset);
        assert_eq!(measured.position, 960);
        assert!(clock.stamp(480, 2400, Some(1920), false).reset);
    }

    #[test]
    fn untimed_discontinuity_after_stall_reanchors_to_arrival() {
        let mut clock = PacketClock::default();
        clock.stamp(480, 480, Some(0), false);
        let stamp = clock.stamp(480, 48_480, None, false);
        assert!(stamp.reset);
        assert_eq!(stamp.start, 48_000);
    }
}
