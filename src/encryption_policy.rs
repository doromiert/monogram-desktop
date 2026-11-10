use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Plain,
    Legacy,
    Omemo2,
}
impl Mode {
    pub fn encrypted(self) -> bool {
        self != Self::Plain
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Plain => "Unencrypted",
            Self::Legacy => "Legacy OMEMO",
            Self::Omemo2 => "OMEMO 2",
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
            Self::Omemo2 => Some(Mode::Omemo2),
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
            .map(|_| self.mode)
    }
}

/// A probe returns false only after a definitive protocol or key-availability
/// result. An I/O, storage or connection error must stop selection.
pub async fn select<E>(
    modern: impl Future<Output = Result<bool, E>>,
    legacy: impl Future<Output = Result<bool, E>>,
) -> Result<Mode, E> {
    if modern.await? {
        Ok(Mode::Omemo2)
    } else if legacy.await? {
        Ok(Mode::Legacy)
    } else {
        Ok(Mode::Plain)
    }
}
