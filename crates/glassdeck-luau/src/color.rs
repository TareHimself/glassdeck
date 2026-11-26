//! `color.hex`, `color.rgb` and `color.hsv`, for every script that builds colours: Luau's checker
//! knows only 3-wide `vector.create`, and colours carry alpha.

use anyhow::Result;
use mlua::{Lua, Table, Vector};

use crate::LuaResultExt;
use crate::options::parse_color;

pub fn color_api(lua: &Lua) -> Result<Table> {
    let color = lua.create_table().lua_context("creating color")?;
    let hex = lua.create_function(|_, hex: String| {
        let [r, g, b, a] = parse_color(&hex).map_err(|e| mlua::Error::runtime(format!("color.hex: {e:#}")))?;
        Ok(Vector::new(r, g, b, a))
    });
    color.set("hex", hex.lua_context("creating color.hex")?).lua_context("installing color.hex")?;
    let rgb = lua.create_function(|_, (r, g, b, a): (f32, f32, f32, Option<f32>)| {
        Ok(Vector::new(r / 255.0, g / 255.0, b / 255.0, a.unwrap_or(255.0) / 255.0))
    });
    color.set("rgb", rgb.lua_context("creating color.rgb")?).lua_context("installing color.rgb")?;
    let hsv_of = lua.create_function(|_, (h, s, v, a): (f32, f32, f32, Option<f32>)| Ok(hsv(h, s, v, a.unwrap_or(1.0))));
    color.set("hsv", hsv_of.lua_context("creating color.hsv")?).lua_context("installing color.hsv")?;
    Ok(color)
}

/// Hue in degrees, saturation and value from 0 to 1.
pub fn hsv(h: f32, s: f32, v: f32, a: f32) -> Vector {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    Vector::new(r + m, g + m, b + m, a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hues_land_on_their_primaries() {
        let red = hsv(0.0, 1.0, 1.0, 1.0);
        assert_eq!((red.x(), red.y(), red.z()), (1.0, 0.0, 0.0));
        let blue = hsv(240.0, 1.0, 1.0, 1.0);
        assert_eq!((blue.x(), blue.y(), blue.z()), (0.0, 0.0, 1.0));
    }
}
