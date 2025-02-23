mod scripts;

use std::time;

#[cfg(feature = "local_accelerate")]
use std::{
    collections::HashMap,
    sync::{LazyLock, RwLock},
};

use scripts::ALLOW_N_SCRIPT;

#[cfg(feature = "local_accelerate")]
static RESET_TIME_STORE: LazyLock<RwLock<HashMap<String, time::Instant>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

const DEFAULT_LIMITER_KEY_PREFIX: &str = "redis_rate:";

#[cfg(feature = "local_accelerate")]
const DEFAULT_LIMITER_EVENT_CHANNEL: &str = "redis_rate_channel";
#[cfg(feature = "local_accelerate")]
const LIMITER_RESET_EVENT_PREFIX: &str = "reset:";

/// Rate limit setting.
#[derive(Debug, Clone)]
pub struct Limit {
    rate: usize,
    burst: usize,
    period_seconds: usize,
}

impl Limit {
    /// Create a new `Limit` setting.
    /// Code will panic if you try to create a limit with invalid values.
    pub fn new(rate: usize, burst: usize, period_seconds: usize) -> Self {
        if period_seconds == 0 {
            panic!("period_seconds must be greater than 0");
        }
        if rate == 0 {
            panic!("rate must be greater than 0");
        }
        if rate > burst {
            panic!("rate must be less than or equal to burst");
        }

        Limit {
            rate,
            burst,
            period_seconds,
        }
    }
}

/// Compile-time checked macro to create a new `Limit` instance.
/// If you want to create dynamically configured limits, use `Limit::new` instead.
#[macro_export]
macro_rules! new_limit {
    ($rate:expr, $burst:expr, $period_seconds:expr) => {{
        const _: () = {
            assert!($period_seconds > 0, "period_seconds must be greater than 0");
            assert!($rate > 0, "rate must be greater than 0");
            assert!($rate <= $burst, "rate must be less than or equal to burst");
        };
        $crate::Limit::new($rate, $burst, $period_seconds)
    }};
}

/// Result of a limit check.
#[derive(Debug, Clone)]
pub struct LimitResult {
    /// Whether the request is limited.
    pub limited: bool,
    /// Remaining requests that can be made within the limit.
    pub remaining: usize,
    /// Duration after which the request can be retried.
    /// If the request is not limited, this will be `None`.
    pub retry_after: Option<time::Duration>,
    /// Duration after which the limit will be totally reset.
    pub reset_after: time::Duration,
}

/// Rate limiter backed by Redis.
#[derive(Debug, Clone)]
pub struct Limiter {
    client: redis::Client,
    key_prefix: String,

    #[cfg(feature = "local_accelerate")]
    event_channel: String,
}

impl Limiter {
    /// Create a new limiter with the given Redis client.
    pub fn new(client: redis::Client) -> Self {
        Limiter {
            client,
            key_prefix: DEFAULT_LIMITER_KEY_PREFIX.to_string(),

            #[cfg(feature = "local_accelerate")]
            event_channel: DEFAULT_LIMITER_EVENT_CHANNEL.to_string(),
        }
    }

    /// Set the key prefix for the limiter's Redis keys.
    pub fn set_key_prefix(mut self, key_prefix: &str) -> Self {
        self.key_prefix = key_prefix.to_string();
        self
    }

    /// Set the event channel name for the limiter.
    /// This should be called before `start_event_sync`.
    #[cfg(feature = "local_accelerate")]
    pub fn set_event_channel(mut self, channel: &str) -> Self {
        self.event_channel = channel.to_string();
        self
    }

    /// Start a listening loop on the event channel.
    /// When reset event is triggered on other instances, the limiter will reset the local cache for the key.
    #[cfg(feature = "local_accelerate")]
    pub fn start_event_sync(&self) -> Result<(), redis::RedisError> {
        let mut con = self.client.get_connection()?;
        let mut pubsub = con.as_pubsub();
        pubsub.subscribe(&self.event_channel).unwrap();
        loop {
            let msg = pubsub.get_message()?.get_payload::<String>()?;
            if msg.starts_with(LIMITER_RESET_EVENT_PREFIX) {
                let payload = msg.split_at(LIMITER_RESET_EVENT_PREFIX.len()).1;
                if let Ok(mut store) = RESET_TIME_STORE.try_write() {
                    store.remove(payload);
                }
            }
        }
    }

    /// Reset the limit for a key.
    pub fn reset(&self, key: &str) -> Result<(), redis::RedisError> {
        let key = format!("{}{}", self.key_prefix, key);
        let mut con = self.client.get_connection()?;
        redis::cmd("DEL").arg(&key).query::<()>(&mut con)?;

        #[cfg(feature = "local_accelerate")]
        {
            let reset_notify = format!("{}{}", LIMITER_RESET_EVENT_PREFIX, key);
            redis::cmd("PUBLISH")
                .arg(self.event_channel.clone())
                .arg(&reset_notify)
                .query::<()>(&mut con)?;
        }

        Ok(())
    }

    /// Allow a request to be made within the limit.
    pub fn allow(&self, key: &str, limit: &Limit) -> Result<LimitResult, redis::RedisError> {
        self.allow_n(key, limit, 1)
    }

    /// Allow n requests to be made within the limit.
    pub fn allow_n(
        &self,
        key: &str,
        limit: &Limit,
        n: usize,
    ) -> Result<LimitResult, redis::RedisError> {
        let key = format!("{}{}", self.key_prefix, key);

        let emission_interval = limit.period_seconds as f64 / limit.rate as f64;
        let tat_increment = emission_interval * n as f64;
        let brust_offset = limit.burst as f64 * emission_interval;

        #[cfg(feature = "local_accelerate")]
        let now = time::Instant::now();
        #[cfg(feature = "local_accelerate")]
        if let Ok(store) = RESET_TIME_STORE.try_read() {
            if let Some(reset_time) = store.get(&key) {
                let reset_after = reset_time.duration_since(now).as_secs_f64();
                let diff: f64 = reset_after + tat_increment - brust_offset;
                if diff > 0.0 {
                    return Ok(LimitResult {
                        limited: true,
                        remaining: f64::floor((brust_offset - reset_after) / emission_interval)
                            as usize,
                        retry_after: Some(time::Duration::from_secs_f64(diff.abs())),
                        reset_after: reset_time.duration_since(now),
                    });
                }
            }
        }

        let mut con = self.client.get_connection()?;
        let result: redis::Value = ALLOW_N_SCRIPT
            .key(&key)
            .arg(emission_interval)
            .arg(brust_offset)
            .arg(tat_increment)
            .arg(n)
            .invoke(&mut con)?;

        let (limited, remaining, retry_after_secs, reset_after_secs): (bool, usize, f64, f64) =
            redis::from_redis_value(&result)?;
        let retry_after = if retry_after_secs < 0.0 {
            None
        } else {
            Some(time::Duration::from_secs_f64(retry_after_secs))
        };
        let reset_after = time::Duration::from_secs_f64(reset_after_secs);

        #[cfg(feature = "local_accelerate")]
        if let Ok(mut store) = RESET_TIME_STORE.try_write() {
            store.insert(key, now + reset_after);
        }

        Ok(LimitResult {
            limited,
            remaining,
            retry_after,
            reset_after,
        })
    }
}

#[test]
fn test_limiter() {
    #[cfg(feature = "local_accelerate")]
    use std::thread;

    let limit = Limit::new(5, 5, 20);
    let key = "test";
    let limiter = Limiter::new(redis::Client::open("redis://127.0.0.1/").unwrap());
    limiter.reset(key).unwrap();

    #[cfg(feature = "local_accelerate")]
    {
        let limiter_clone = limiter.clone();
        thread::spawn(move || {
            limiter_clone.start_event_sync().unwrap();
        });
    }

    let result = limiter.allow_n(key, &limit, 4).unwrap();
    assert_eq!(result.limited, false);
    assert_eq!(result.remaining, 1);
    let result = limiter.allow_n(key, &limit, 3).unwrap();
    assert_eq!(result.limited, true);
    assert_eq!(result.remaining, 1);
    let result = limiter.allow(&key, &limit).unwrap();
    assert_eq!(result.limited, false);
    assert_eq!(result.remaining, 0);

    let result = limiter.allow_n(key, &limit, 5).unwrap();
    assert_eq!(result.limited, true);
    limiter.reset(key).unwrap();

    #[cfg(feature = "local_accelerate")]
    // Wait for the reset event to be processed in the other thread
    thread::sleep(time::Duration::from_millis(100));

    let result = limiter.allow_n(key, &limit, 5).unwrap();
    assert_eq!(result.limited, false);
}
