//! Lua-scripted rate-control algorithm.
//!
//! The Lua module is expected to return a table `M` with two functions:
//!
//! ```lua
//! local M = {}
//!
//! --- Called once at startup with the [algorithm.lua] config table
//! --- (the `script` key is stripped; all other keys pass through).
//! function M.init(config) end
//!
//! --- Called every tick. `ctx` contains the fields described in RateContext.
//! --- Must return { dl_rate = number, ul_rate = number, trigger_reselect = bool }.
//! function M.calculate(ctx) end
//!
//! return M
//! ```

use super::{RateAlgorithm, RateContext, RateResult};
use crate::config::LuaConfig;
use anyhow::{Context, Result};
use mlua::prelude::*;

pub struct LuaAlgorithm {
    lua: Lua,
    module_key: LuaRegistryKey,
    min_change_interval: f64,
    initial_dl: f64,
    initial_ul: f64,
}

fn lua_err(e: LuaError) -> anyhow::Error {
    anyhow::anyhow!("Lua error: {e}")
}

impl LuaAlgorithm {
    pub fn new(config: &LuaConfig, base_dl: f64, base_ul: f64) -> Result<Self> {
        let lua = Lua::new();

        // Load the script file
        let script = std::fs::read_to_string(&config.script)
            .with_context(|| format!("failed to read Lua script: {}", config.script))?;

        // Execute the script; it must return a module table M
        let module: LuaTable = lua
            .load(&script)
            .set_name(&config.script)
            .eval()
            .map_err(lua_err)
            .with_context(|| format!("failed to evaluate Lua script: {}", config.script))?;

        // Build config table to pass to M.init (all keys except 'script')
        let cfg_table = lua.create_table().map_err(lua_err)?;
        for (k, v) in &config.extra {
            let lua_val = toml_value_to_lua(&lua, v)?;
            cfg_table.set(k.as_str(), lua_val).map_err(lua_err)?;
        }
        cfg_table.set("base_dl_rate", base_dl).map_err(lua_err)?;
        cfg_table.set("base_ul_rate", base_ul).map_err(lua_err)?;

        // Call M.init(config)
        let init_fn: LuaFunction = module
            .get("init")
            .map_err(lua_err)
            .context("Lua module must export an 'init' function")?;
        init_fn
            .call::<()>(cfg_table)
            .map_err(lua_err)
            .context("M.init(config) failed")?;

        // Read optional min_change_interval from the extra config
        let min_change_interval = config
            .extra
            .get("min_change_interval")
            .and_then(|v| v.as_float())
            .unwrap_or(0.5);

        // Verify M.calculate exists before we start the control loop
        let _: LuaFunction = module
            .get("calculate")
            .map_err(lua_err)
            .context("Lua module must export a 'calculate' function")?;

        // Store the module table in the Lua registry
        let module_key = lua.create_registry_value(module).map_err(lua_err)?;

        Ok(Self {
            lua,
            module_key,
            min_change_interval,
            initial_dl: base_dl * 0.6,
            initial_ul: base_ul * 0.6,
        })
    }

    fn calculate_inner(&self, ctx: &RateContext) -> Result<RateResult> {
        let module: LuaTable = self.lua.registry_value(&self.module_key).map_err(lua_err)?;
        let calc_fn: LuaFunction = module.get("calculate").map_err(lua_err)?;

        let lua = &self.lua;
        let ctx_tbl = lua.create_table().map_err(lua_err)?;

        // Populate dl_deltas / ul_deltas as Lua arrays (1-indexed)
        let dl_arr = lua.create_table().map_err(lua_err)?;
        for (i, &v) in ctx.dl_deltas.iter().enumerate() {
            dl_arr.set(i + 1, v).map_err(lua_err)?;
        }
        let ul_arr = lua.create_table().map_err(lua_err)?;
        for (i, &v) in ctx.ul_deltas.iter().enumerate() {
            ul_arr.set(i + 1, v).map_err(lua_err)?;
        }

        ctx_tbl.set("dl_deltas", dl_arr).map_err(lua_err)?;
        ctx_tbl.set("ul_deltas", ul_arr).map_err(lua_err)?;
        ctx_tbl.set("current_dl_rate", ctx.current_dl_rate).map_err(lua_err)?;
        ctx_tbl.set("current_ul_rate", ctx.current_ul_rate).map_err(lua_err)?;
        ctx_tbl.set("dl_utilisation", ctx.dl_utilisation).map_err(lua_err)?;
        ctx_tbl.set("ul_utilisation", ctx.ul_utilisation).map_err(lua_err)?;
        ctx_tbl.set("elapsed_secs", ctx.elapsed_secs).map_err(lua_err)?;
        ctx_tbl.set("base_dl_rate", ctx.base_dl_rate).map_err(lua_err)?;
        ctx_tbl.set("base_ul_rate", ctx.base_ul_rate).map_err(lua_err)?;
        ctx_tbl.set("min_dl_rate", ctx.min_dl_rate).map_err(lua_err)?;
        ctx_tbl.set("min_ul_rate", ctx.min_ul_rate).map_err(lua_err)?;

        let result: LuaTable = calc_fn.call(ctx_tbl).map_err(lua_err)?;

        let dl_rate: f64 = result
            .get("dl_rate")
            .map_err(lua_err)
            .context("Lua result missing 'dl_rate'")?;
        let ul_rate: f64 = result
            .get("ul_rate")
            .map_err(lua_err)
            .context("Lua result missing 'ul_rate'")?;
        let trigger_reselect: bool = result.get("trigger_reselect").unwrap_or(false);

        Ok(RateResult {
            dl_rate: dl_rate.max(ctx.min_dl_rate).floor(),
            ul_rate: ul_rate.max(ctx.min_ul_rate).floor(),
            trigger_reselect,
        })
    }
}

impl RateAlgorithm for LuaAlgorithm {
    fn initial_rates(&self) -> (f64, f64) {
        (self.initial_dl, self.initial_ul)
    }

    fn min_change_interval(&self) -> f64 {
        self.min_change_interval
    }

    fn calculate(&mut self, ctx: &RateContext) -> RateResult {
        match self.calculate_inner(ctx) {
            Ok(r) => r,
            Err(e) => {
                log::error!("Lua calculate error: {e:#} — holding current rates");
                RateResult {
                    dl_rate: ctx.current_dl_rate,
                    ul_rate: ctx.current_ul_rate,
                    trigger_reselect: false,
                }
            }
        }
    }
}

// ── TOML → Lua value conversion ───────────────────────────────────────────────

fn toml_value_to_lua(lua: &Lua, v: &toml::Value) -> Result<LuaValue> {
    use toml::Value;
    Ok(match v {
        Value::String(s) => LuaValue::String(lua.create_string(s.as_bytes()).map_err(lua_err)?),
        Value::Integer(i) => LuaValue::Integer(*i),
        Value::Float(f) => LuaValue::Number(*f),
        Value::Boolean(b) => LuaValue::Boolean(*b),
        Value::Array(arr) => {
            let t = lua.create_table().map_err(lua_err)?;
            for (i, item) in arr.iter().enumerate() {
                t.set(i + 1, toml_value_to_lua(lua, item)?).map_err(lua_err)?;
            }
            LuaValue::Table(t)
        }
        Value::Table(map) => {
            let t = lua.create_table().map_err(lua_err)?;
            for (k, val) in map {
                t.set(k.as_str(), toml_value_to_lua(lua, val)?).map_err(lua_err)?;
            }
            LuaValue::Table(t)
        }
        Value::Datetime(dt) => {
            LuaValue::String(lua.create_string(dt.to_string().as_bytes()).map_err(lua_err)?)
        }
    })
}
