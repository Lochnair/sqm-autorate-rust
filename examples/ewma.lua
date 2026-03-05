-- examples/ewma.lua
--
-- Lua port of the SqmEwmaAlgorithm — serves as a porting template and
-- smoke-test for the Lua algorithm plugin interface.
--
-- Enable with:
--   [algorithm]
--   type = "lua"
--
--   [algorithm.lua]
--   script               = "/etc/sqm-autorate/ewma.lua"
--   download_delay_ms    = 15.0
--   upload_delay_ms      = 15.0
--   high_load_level      = 0.8
--   speed_hist_size      = 100

local M = {}

-- ── Internal state ────────────────────────────────────────────────────────────

local cfg = {
    download_delay_ms = 15.0,
    upload_delay_ms   = 15.0,
    high_load_level   = 0.8,
    speed_hist_size   = 100,
    base_dl           = 0,
    base_ul           = 0,
    min_dl            = 0,
    min_ul            = 0,
}

local function make_dir_state(base_rate, size)
    local rates = {}
    for i = 1, size do
        rates[i] = (math.random() * 0.2 + 0.75) * base_rate
    end
    return { safe_rates = rates, nrate = 1 }
end

local dl_state = nil
local ul_state = nil

-- ── Helpers ───────────────────────────────────────────────────────────────────

local function max_in(t)
    local m = -math.huge
    for _, v in ipairs(t) do
        if v > m then m = v end
    end
    return m
end

--- Compute the new rate for one direction.
--- Returns the new rate (kbit/s), clamped to min_rate.
local function calc_direction(deltas, current_rate, utilisation, delay_ms,
                               high_load_level, base_rate, min_rate, state)
    if #deltas == 0 then return min_rate end

    local delta_stat = (#deltas >= 3) and deltas[3] or deltas[1]
    local next_rate  = current_rate

    if delta_stat > 0.0 then
        local load = (current_rate > 0) and (utilisation / current_rate) or 0.0

        if delta_stat < delay_ms and load > high_load_level then
            -- No significant delay + high utilisation → increase
            state.safe_rates[state.nrate] = math.floor(current_rate * load)
            local max_safe = max_in(state.safe_rates)
            next_rate = current_rate
                * (1.0 + 0.1 * math.max(0.0, 1.0 - current_rate / max_safe))
                + base_rate * 0.03
            state.nrate = (state.nrate % #state.safe_rates) + 1
        end

        if delta_stat > delay_ms then
            -- Delay → decrease toward random previously-safe rate
            local rnd_idx  = math.random(#state.safe_rates)
            local rnd_rate = state.safe_rates[rnd_idx]
            local load_cl  = (current_rate > 0) and (utilisation / current_rate) or 0.0
            next_rate = math.min(rnd_rate, 0.9 * current_rate * load_cl)
        end
    end

    return math.max(math.floor(next_rate), min_rate)
end

-- ── Plugin interface ──────────────────────────────────────────────────────────

--- Called once at startup with the [algorithm.lua] config table.
function M.init(config)
    cfg.download_delay_ms = config.download_delay_ms or cfg.download_delay_ms
    cfg.upload_delay_ms   = config.upload_delay_ms   or cfg.upload_delay_ms
    cfg.high_load_level   = config.high_load_level   or cfg.high_load_level
    cfg.speed_hist_size   = config.speed_hist_size   or cfg.speed_hist_size
    cfg.base_dl           = config.base_dl_rate      or 0
    cfg.base_ul           = config.base_ul_rate      or 0
    cfg.min_dl            = cfg.base_dl * 0.2
    cfg.min_ul            = cfg.base_ul * 0.2

    math.randomseed(os.time())
    dl_state = make_dir_state(cfg.base_dl, cfg.speed_hist_size)
    ul_state = make_dir_state(cfg.base_ul, cfg.speed_hist_size)
end

--- Called every control-loop tick. ctx contains:
---   dl_deltas, ul_deltas       — sorted arrays (ascending) of OWD deltas
---   current_dl_rate            — current DL shaper rate (kbit/s)
---   current_ul_rate
---   dl_utilisation             — measured DL throughput (kbit/s)
---   ul_utilisation
---   base_dl_rate, base_ul_rate — configured base rates
---   min_dl_rate,  min_ul_rate  — minimum allowed rates
---   elapsed_secs               — seconds since last tick
---
--- Must return: { dl_rate = ..., ul_rate = ..., trigger_reselect = bool }
function M.calculate(ctx)
    local dl_rate = calc_direction(
        ctx.dl_deltas,
        ctx.current_dl_rate,
        ctx.dl_utilisation,
        cfg.download_delay_ms,
        cfg.high_load_level,
        ctx.base_dl_rate,
        ctx.min_dl_rate,
        dl_state
    )
    local ul_rate = calc_direction(
        ctx.ul_deltas,
        ctx.current_ul_rate,
        ctx.ul_utilisation,
        cfg.upload_delay_ms,
        cfg.high_load_level,
        ctx.base_ul_rate,
        ctx.min_ul_rate,
        ul_state
    )

    local trigger = (#ctx.dl_deltas < 5) or (#ctx.ul_deltas < 5)

    return { dl_rate = dl_rate, ul_rate = ul_rate, trigger_reselect = trigger }
end

return M
