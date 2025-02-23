use std::sync::LazyLock;

pub(crate) static ALLOW_N_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    // This is an edited version of the script from the redis-gcra project:
    // Copyright (c) 2017 Pavel Pravosud
    // https://github.com/rwz/redis-gcra/blob/master/vendor/perform_gcra_ratelimit.lua
    redis::Script::new(
        r#"-- this script has side-effects, so it requires replicate commands mode
redis.replicate_commands()

local rate_limit_key = KEYS[1]
local emission_interval = ARGV[1]
local burst_offset = ARGV[2]
local tat_increment = ARGV[3]
local cost = ARGV[4]

-- redis returns time as an array containing two integers: seconds of the epoch
-- time (10 digits) and microseconds (6 digits). for convenience we need to
-- convert them to a floating point number. the resulting number is 16 digits,
-- bordering on the limits of a 64-bit double-precision floating point number.
-- adjust the epoch to be relative to Jan 1, 2017 00:00:00 GMT to avoid floating
-- point problems. this approach is good until "now" is 2,483,228,799 (Wed, 09
-- Sep 2048 01:46:39 GMT), when the adjusted value is 16 digits.
local redis_now = redis.call("TIME")
local jan_1_2017 = 1483228800
local now = (redis_now[1] - jan_1_2017) + (redis_now[2] / 1000000)

local tat = redis.call("GET", rate_limit_key)
if not tat then
  tat = now
else
  tat = tonumber(tat)
end
local new_tat = math.max(tat, now) + tat_increment
local allow_at = new_tat - burst_offset

local limited
local remaining
local retry_after
local reset_after

if allow_at > now then
  limited = true
  remaining = math.floor((now - tat + burst_offset) / emission_interval)
  retry_after = allow_at - now
  reset_after = tat - now
else
  limited = false
  remaining = math.floor((now - allow_at) / emission_interval)
  retry_after = -1
  reset_after = new_tat - now
  redis.call("SET", rate_limit_key, new_tat, "EX", math.ceil(reset_after))
end

return {limited, remaining, retry_after, reset_after}
"#,
    )
});
