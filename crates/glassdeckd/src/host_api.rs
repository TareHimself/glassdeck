//! Host-only Luau APIs for `host.luau`: live resources for the keyboard half, image fitting so the
//! keyboard only ever decodes small PNGs, and what's playing.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Cursor;
use std::rc::Rc;

use anyhow::{Context, Result, ensure};
use glassdeck_luau::{LiveName, LuaResultExt, MAX_LIVE_RESOURCE_BYTES};
use image::ImageFormat;
use image::imageops::FilterType;
use mlua::{Lua, Table, Value};

use crate::media::SharedNowPlaying;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
/// Larger than any widget can be, so a script asking for more has made a mistake.
const MAX_FIT_SIDE: u32 = 2048;

/// One widget's live resources not yet handed to the Link; the latest per name wins.
pub type LiveOutbox = Rc<RefCell<BTreeMap<String, Vec<u8>>>>;

/// `live.put(name, png)` and `live.clear(name)`.
pub fn live_api(lua: &Lua, outbox: &LiveOutbox) -> Result<Table> {
    let api = lua.create_table().lua_context("creating live")?;
    let putting = Rc::clone(outbox);
    let put = lua
        .create_function(move |_, (name, png): (String, mlua::LuaString)| {
            let name = checked(&name, &png.as_bytes()).map_err(|e| mlua::Error::runtime(format!("live.put: {e:#}")))?;
            putting.borrow_mut().insert(name.as_str().to_owned(), png.as_bytes().to_vec());
            Ok(())
        })
        .lua_context("creating live.put")?;
    let clearing = Rc::clone(outbox);
    let clear = lua
        .create_function(move |_, name: String| {
            let name = LiveName::parse(&name).map_err(|e| mlua::Error::runtime(format!("live.clear: {e:#}")))?;
            clearing.borrow_mut().insert(name.as_str().to_owned(), Vec::new());
            Ok(())
        })
        .lua_context("creating live.clear")?;
    api.set("put", put).lua_context("filling live")?;
    api.set("clear", clear).lua_context("filling live")?;
    Ok(api)
}

/// What the keyboard will accept: a live name, a PNG, within the size limit.
fn checked(name: &str, png: &[u8]) -> Result<LiveName> {
    let name = LiveName::parse(name)?;
    ensure!(png.starts_with(PNG_SIGNATURE), "{} isn't a PNG; image.fit makes one", name.as_str());
    ensure!(png.len() <= MAX_LIVE_RESOURCE_BYTES, "{} is {} bytes, over the {MAX_LIVE_RESOURCE_BYTES} limit; image.fit makes it smaller", name.as_str(), png.len());
    Ok(name)
}

/// How `image.fit` and `image.fill` resize.
#[derive(Clone, Copy)]
enum Resize {
    /// Within w x h, keeping the shape
    Fit,
    /// Exactly w x h, keeping the shape by cropping the middle
    Fill,
}

/// `image.fit(bytes, w, h)` and `image.fill(bytes, w, h)`: a PNG or JPEG resized, as a PNG.
pub fn image_api(lua: &Lua) -> Result<Table> {
    let api = lua.create_table().lua_context("creating image")?;
    for (name, how) in [("fit", Resize::Fit), ("fill", Resize::Fill)] {
        let function = lua
            .create_function(move |lua, (bytes, w, h): (mlua::LuaString, u32, u32)| {
                let png = resize(&bytes.as_bytes(), w, h, how).map_err(|e| mlua::Error::runtime(format!("image.{name}: {e:#}")))?;
                lua.create_string(png)
            })
            .lua_context(&format!("creating image.{name}"))?;
        api.set(name, function).lua_context("filling image")?;
    }
    Ok(api)
}

/// `media.current()`: nil when nothing is playing or paused, else
/// `{ title, artist, album, playing, track, cover? }`. `track` changes with the track.
pub fn media_api(lua: &Lua, now_playing: &SharedNowPlaying) -> Result<Table> {
    let api = lua.create_table().lua_context("creating media")?;
    let reading = SharedNowPlaying::clone(now_playing);
    let current = lua
        .create_function(move |lua, ()| {
            let Some(now) = reading.lock().expect("media poisoned").clone() else { return Ok(Value::Nil) };
            let track = lua.create_table()?;
            track.set("title", now.track.title)?;
            track.set("artist", now.track.artist)?;
            track.set("album", now.track.album)?;
            track.set("playing", now.playing)?;
            track.set("track", now.track_number)?;
            if let Some(cover) = now.cover {
                track.set("cover", lua.create_string(cover.as_slice())?)?;
            }
            Ok(Value::Table(track))
        })
        .lua_context("creating media.current")?;
    api.set("current", current).lua_context("filling media")?;
    Ok(api)
}

fn resize(bytes: &[u8], w: u32, h: u32, how: Resize) -> Result<Vec<u8>> {
    ensure!((1..=MAX_FIT_SIDE).contains(&w) && (1..=MAX_FIT_SIDE).contains(&h), "{w}x{h} must be within 1..={MAX_FIT_SIDE} on each side");
    let image = image::load_from_memory(bytes).context("decoding the image (PNG or JPEG)")?;
    let fitted = match how {
        Resize::Fit => image.resize(w, h, FilterType::Triangle),
        Resize::Fill => image.resize_to_fill(w, h, FilterType::Triangle),
    };
    let mut png = Vec::new();
    fitted.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).context("encoding the PNG")?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbaImage::from_pixel(w, h, image::Rgba([0, 200, 0, 255])).write_to(&mut Cursor::new(&mut out), ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn fit_keeps_the_shape_within_the_box() {
        let fitted = image::load_from_memory(&resize(&png(400, 200), 100, 100, Resize::Fit).unwrap()).unwrap();
        assert_eq!((fitted.width(), fitted.height()), (100, 50));
        let filled = image::load_from_memory(&resize(&png(400, 200), 100, 100, Resize::Fill).unwrap()).unwrap();
        assert_eq!((filled.width(), filled.height()), (100, 100));
        assert!(resize(&png(4, 4), 0, 10, Resize::Fit).is_err());
        assert!(resize(b"not an image", 10, 10, Resize::Fit).is_err());
    }

    #[test]
    fn live_put_takes_only_named_pngs_and_the_latest_wins() {
        let lua = Lua::new();
        let outbox: LiveOutbox = Rc::default();
        lua.globals().set("live", live_api(&lua, &outbox).unwrap()).unwrap();
        lua.globals().set("first", lua.create_string(png(2, 2)).unwrap()).unwrap();
        lua.globals().set("second", lua.create_string(png(3, 3)).unwrap()).unwrap();
        lua.load(r#"live.put("cover", first); live.put("cover", second)"#).exec().unwrap();
        assert_eq!(outbox.borrow()["cover"], png(3, 3));
        lua.load(r#"live.clear("cover")"#).exec().unwrap();
        assert!(outbox.borrow()["cover"].is_empty());
        assert!(lua.load(r#"live.put("cover", "jpeg bytes")"#).exec().is_err());
        assert!(lua.load(r#"live.put("a/b", first)"#).exec().is_err());
    }
}
