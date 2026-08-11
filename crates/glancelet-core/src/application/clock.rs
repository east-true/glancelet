use std::sync::RwLock;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use crate::{GlanceletError, Result};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

pub struct FixedClock {
    now: RwLock<DateTime<Utc>>,
}

impl FixedClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: RwLock::new(now),
        }
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *self.now.write().expect("fixed clock poisoned") = now;
    }
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.read().expect("fixed clock poisoned")
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TimeContext {
    timezone: Tz,
}

impl TimeContext {
    pub fn system() -> Result<Self> {
        let name = iana_time_zone::get_timezone().map_err(|error| {
            GlanceletError::InvalidOperation(format!("cannot determine local timezone: {error}"))
        })?;
        Self::named(&name)
    }

    pub fn named(name: &str) -> Result<Self> {
        let timezone = name.parse::<Tz>().map_err(|_| {
            GlanceletError::InvalidOperation(format!("unknown IANA timezone: {name}"))
        })?;
        Ok(Self { timezone })
    }

    pub fn local_date(&self, instant: DateTime<Utc>) -> chrono::NaiveDate {
        instant.with_timezone(&self.timezone).date_naive()
    }

    pub fn timezone(&self) -> Tz {
        self.timezone
    }
}
