//! `json.decode` and `json.encode`: JSON text to Luau values and back. Objects and arrays both become
//! tables; a table whose keys are exactly 1 to n encodes as an array, any other as an object.

use anyhow::{Result, anyhow, bail, ensure};
use mlua::{Lua, Table, Value};
use serde_json::{Map, Number};

use crate::{LuaResultExt, MAX_DEPTH};

/// Installs nothing itself: the host sets the returned table as its `json` global.
pub fn json_api(lua: &Lua) -> Result<Table> {
    let api = lua.create_table().lua_context("creating json")?;
    let decode = lua
        .create_function(|lua, text: mlua::LuaString| {
            let parsed: serde_json::Value = serde_json::from_slice(&text.as_bytes()).map_err(|e| mlua::Error::runtime(format!("json.decode: {e}")))?;
            to_lua(lua, &parsed, 0).map_err(|e| mlua::Error::runtime(format!("json.decode: {e:#}")))
        })
        .lua_context("creating json.decode")?;
    let encode = lua
        .create_function(|_, value: Value| {
            let json = from_lua(&value, 0).map_err(|e| mlua::Error::runtime(format!("json.encode: {e:#}")))?;
            Ok(json.to_string())
        })
        .lua_context("creating json.encode")?;
    api.raw_set("decode", decode).lua_context("creating json")?;
    api.raw_set("encode", encode).lua_context("creating json")?;
    Ok(api)
}

/// `null` becomes `nil`, so an object's null field reads as missing.
fn to_lua(lua: &Lua, json: &serde_json::Value, depth: usize) -> Result<Value> {
    ensure!(depth < MAX_DEPTH, "nests deeper than {MAX_DEPTH}");
    Ok(match json {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Number(n.as_f64().ok_or_else(|| anyhow!("{n} isn't a number Luau can hold"))?),
        },
        serde_json::Value::String(s) => Value::String(lua.create_string(s).lua_context("reading a string")?),
        serde_json::Value::Array(items) => {
            let table = lua.create_table_with_capacity(items.len(), 0).lua_context("reading an array")?;
            for (i, item) in items.iter().enumerate() {
                table.raw_set(i + 1, to_lua(lua, item, depth + 1)?).lua_context("reading an array")?;
            }
            Value::Table(table)
        }
        serde_json::Value::Object(fields) => {
            let table = lua.create_table_with_capacity(0, fields.len()).lua_context("reading an object")?;
            for (key, value) in fields {
                table.raw_set(key.as_str(), to_lua(lua, value, depth + 1)?).lua_context("reading an object")?;
            }
            Value::Table(table)
        }
    })
}

/// An empty table encodes as `{}`.
fn from_lua(value: &Value, depth: usize) -> Result<serde_json::Value> {
    ensure!(depth < MAX_DEPTH, "tables nest deeper than {MAX_DEPTH}, or one holds itself");
    Ok(match value {
        Value::Nil => serde_json::Value::Null,
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::Integer(i) => serde_json::Value::from(*i),
        Value::Number(n) => serde_json::Value::Number(Number::from_f64(*n).ok_or_else(|| anyhow!("{n} has no JSON form"))?),
        Value::String(s) => serde_json::Value::String(s.to_str().map_err(|_| anyhow!("a string that isn't UTF-8 has no JSON form"))?.to_owned()),
        Value::Table(table) => {
            let pairs: Vec<(Value, Value)> = table.pairs::<Value, Value>().collect::<mlua::Result<_>>().lua_context("reading a table")?;
            let length = table.raw_len();
            let is_array = !pairs.is_empty()
                && pairs.len() == length
                && pairs.iter().all(|(k, _)| matches!(k, Value::Integer(i) if *i >= 1 && *i as usize <= length));
            if is_array {
                let mut items = vec![serde_json::Value::Null; length];
                for (key, value) in &pairs {
                    let Value::Integer(i) = key else { unreachable!("checked above") };
                    items[*i as usize - 1] = from_lua(value, depth + 1)?;
                }
                serde_json::Value::Array(items)
            } else {
                let mut fields = Map::new();
                for (key, value) in &pairs {
                    let Value::String(key) = key else {
                        bail!("a table with {} keys besides 1 to n has no JSON form; object keys must be strings", key.type_name())
                    };
                    let key = key.to_str().map_err(|_| anyhow!("an object key that isn't UTF-8"))?.to_owned();
                    fields.insert(key, from_lua(value, depth + 1)?);
                }
                serde_json::Value::Object(fields)
            }
        }
        other => bail!("a {} has no JSON form", other.type_name()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vm;

    fn vm() -> Vm {
        let vm = Vm::new().unwrap();
        vm.lua().globals().set("json", json_api(vm.lua()).unwrap()).unwrap();
        vm.seal().unwrap();
        vm
    }

    #[test]
    fn json_reads_into_tables_and_writes_back() {
        let vm = vm();
        let ok: bool = vm
            .load(
                r#"local t = json.decode('{"current":{"cloud":40,"rain":0.5,"tags":["a","b"],"gone":null}}')
                return t.current.cloud == 40 and t.current.rain == 0.5 and t.current.tags[2] == "b" and t.current.gone == nil"#,
                "t",
            )
            .unwrap();
        assert!(ok);
        let text: String = vm.load(r#"return json.encode({ list = { 1, 2.5, "x" }, on = true })"#, "t").unwrap();
        let back: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back, serde_json::json!({ "list": [1, 2.5, "x"], "on": true }));
        assert_eq!(vm.load::<String>("return json.encode({})", "t").unwrap(), "{}");
    }

    #[test]
    fn what_json_cant_hold_is_refused_by_name() {
        let vm = vm();
        let error = |source: &str| format!("{:#}", vm.load::<Value>(source, "t").unwrap_err());
        assert!(error("return json.encode({ f = print })").contains("function has no JSON form"));
        assert!(error("return json.encode({ [true] = 1 })").contains("object keys must be strings"));
        assert!(error("return json.encode(0/0)").contains("has no JSON form"));
        assert!(error("local t = {} t.self = t return json.encode(t)").contains("holds itself"));
        assert!(error("return json.decode('{nope')").contains("json.decode"));
    }
}
