use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serializer};

fn duration_since_epoch(time: &SystemTime) -> Result<Duration, String> {
    time.duration_since(UNIX_EPOCH)
        .map_err(|err| format!("time occurs before UNIX_EPOCH: {err}"))
}

pub fn serialize<S>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let millis = duration_since_epoch(time)
        .map_err(serde::ser::Error::custom)?
        .as_millis();
    serializer.serialize_u128(millis)
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
where
    D: Deserializer<'de>,
{
    let millis = u128::deserialize(deserializer)?;
    // Timestamps beyond u64::MAX ms (~584 million years from epoch) are impossible; clamp to max
    let millis_u64 = millis
        .min(u128::from(u64::MAX))
        .try_into()
        .unwrap_or(u64::MAX);
    Ok(UNIX_EPOCH + Duration::from_millis(millis_u64))
}

pub mod option {
    use super::{
        Deserialize, Deserializer, Duration, Serializer, SystemTime, UNIX_EPOCH,
        duration_since_epoch,
    };

    #[allow(clippy::ref_option)] // Serde `with` signature requires `&Option<T>`.
    pub fn serialize<S>(time: &Option<SystemTime>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match time {
            Some(value) => serializer.serialize_some(
                &duration_since_epoch(value)
                    .map_err(serde::ser::Error::custom)?
                    .as_millis(),
            ),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<SystemTime>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let option = Option::<u128>::deserialize(deserializer)?;
        Ok(option.map(|millis| {
            // Timestamps beyond u64::MAX ms (~584 million years from epoch) are impossible; clamp to max
            let millis_u64 = millis
                .min(u128::from(u64::MAX))
                .try_into()
                .unwrap_or(u64::MAX);
            UNIX_EPOCH + Duration::from_millis(millis_u64)
        }))
    }
}
