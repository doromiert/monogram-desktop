use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Plain,
    Legacy,
    Omemo2,
    Pq,
}
impl Mode {
    pub fn encrypted(self) -> bool {
        self != Self::Plain
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Plain => "Unencrypted",
            Self::Legacy => "Legacy OMEMO",
            Self::Omemo2 => "Classic OMEMO2",
            Self::Pq => "OMEMO2",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preference {
    #[default]
    Automatic,
    Plain,
    Legacy,
    Omemo2,
}
impl Preference {
    pub const OPTIONS: [Self; 4] = [Self::Automatic, Self::Omemo2, Self::Legacy, Self::Plain];
    pub fn forced(self) -> Option<Mode> {
        match self {
            Self::Automatic => None,
            Self::Plain => Some(Mode::Plain),
            Self::Legacy => Some(Mode::Legacy),
            Self::Omemo2 => Some(Mode::Pq),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Selection {
    pub mode: Mode,
    pub time_ms: i64,
}
impl Selection {
    pub fn current(self, now: i64) -> Option<Mode> {
        const DAY_MS: i64 = 24 * 60 * 60 * 1000;
        now.checked_sub(self.time_ms)
            .filter(|age| (0..DAY_MS).contains(age))
            // Published PQ device lists can outlive their bundles; preflight each send.
            .filter(|_| !matches!(self.mode, Mode::Omemo2 | Mode::Pq))
            .map(|_| self.mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pq_selection_is_rechecked_while_explicit_choices_remain_pinned() {
        let now = 1_000_000;
        assert_eq!(Selection { mode: Mode::Pq, time_ms: now }.current(now), None);
        assert_eq!(Selection { mode: Mode::Legacy, time_ms: now }.current(now), Some(Mode::Legacy));
        assert_eq!(Preference::Omemo2.forced(), Some(Mode::Pq));
    }
}

/// A probe returns false only after a definitive protocol or key-availability
/// result. An I/O, storage or connection error must stop selection.
pub async fn select<E>(
    pq: impl Future<Output = Result<bool, E>>,
    legacy: impl Future<Output = Result<bool, E>>,
) -> Result<Mode, E> {
    if pq.await? {
        Ok(Mode::Pq)
    } else if legacy.await? {
        Ok(Mode::Legacy)
    } else {
        Ok(Mode::Plain)
    }
}
