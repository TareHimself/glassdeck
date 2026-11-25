//! Resource handles: `keyboard.resource(name)`, `host.resource(name)` and, on the keyboard,
//! `live.resource(name)` return an opaque `Resource`, with the name checked when the handle is made.
//! Each host installs only the tables for resources it has, so `keyboard.luau` can't even name a host
//! resource.

use std::fmt;
use std::rc::Rc;

use anyhow::{Result, ensure};
use glassdeck_package::{Resource, ResourceKind, ResourceName};
use mlua::{AnyUserData, MetaMethod, UserData, UserDataMethods};

use crate::{LuaResultExt, Vm};

/// Live resources travel in one Link message; anything bigger should be resized on the host first.
pub const MAX_LIVE_RESOURCE_BYTES: usize = 1024 * 1024;
const MAX_LIVE_NAME_BYTES: usize = 64;

/// Reads a resource's bytes for `Resource:read()`.
pub type ResourceReader = Rc<dyn Fn(&Resource) -> Result<Vec<u8>>>;

/// A live resource's name: one segment of the characters a resource name allows.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LiveName(String);

impl LiveName {
    pub fn parse(name: &str) -> Result<Self> {
        ensure!(!name.contains('/') && name.len() <= MAX_LIVE_NAME_BYTES, "{name:?} isn't a live resource name: one word of up to {MAX_LIVE_NAME_BYTES} characters");
        ResourceName::parse(name)?;
        Ok(Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a handle points at: a file in the module, or a resource the host half sent at run time.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ResourceRef {
    Packaged(Resource),
    Live(LiveName),
}

impl fmt::Display for ResourceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Packaged(resource) => resource.fmt(f),
            Self::Live(name) => write!(f, "live.resource({:?})", name.as_str()),
        }
    }
}

struct Handle(ResourceRef);

impl UserData for Handle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("read", |lua, handle, ()| {
            let ResourceRef::Packaged(resource) = &handle.0 else {
                return Err(mlua::Error::runtime(format!("{} can only be drawn, not read", handle.0)));
            };
            let reader = lua.app_data_ref::<ResourceReader>().ok_or_else(|| mlua::Error::runtime("resources can't be read here"))?;
            let bytes = reader(resource).map_err(|e| mlua::Error::runtime(format!("{e:#}")))?;
            lua.create_string(bytes)
        });
        methods.add_meta_method(MetaMethod::ToString, |_, handle, ()| Ok(handle.0.to_string()));
    }
}

/// What a handle points at, for host APIs that take one, like `ui:image`.
pub fn resource_of(value: &AnyUserData) -> mlua::Result<ResourceRef> {
    match value.borrow::<Handle>() {
        Ok(handle) => Ok(handle.0.clone()),
        Err(_) => Err(mlua::Error::runtime("expected a resource from keyboard.resource(...) or live.resource(...)")),
    }
}

impl Vm {
    /// Installs a `resource(name)` table for each kind, named after it, and the reader behind `:read()`.
    pub fn install_resources(&self, kinds: &[ResourceKind], reader: ResourceReader) -> Result<()> {
        let lua = self.lua();
        for &kind in kinds {
            let resource = lua
                .create_function(move |_, name: String| {
                    let name = ResourceName::parse(&name).map_err(|e| mlua::Error::runtime(format!("{}.resource: {e:#}", kind.folder())))?;
                    Ok(Handle(ResourceRef::Packaged(Resource { kind, name })))
                })
                .lua_context("creating resource")?;
            let table = lua.create_table().lua_context("creating a resource table")?;
            table.set("resource", resource).lua_context("filling a resource table")?;
            lua.globals().set(kind.folder(), table).lua_context("installing a resource table")?;
        }
        lua.set_app_data(reader);
        Ok(())
    }

    /// Installs `live.resource(name)` for the keyboard half: a handle to what the host half sends.
    pub fn install_live_resources(&self) -> Result<()> {
        let lua = self.lua();
        let resource = lua
            .create_function(|_, name: String| {
                let name = LiveName::parse(&name).map_err(|e| mlua::Error::runtime(format!("live.resource: {e:#}")))?;
                Ok(Handle(ResourceRef::Live(name)))
            })
            .lua_context("creating live.resource")?;
        let table = lua.create_table().lua_context("creating live")?;
        table.set("resource", resource).lua_context("filling live")?;
        lua.globals().set("live", table).lua_context("installing live")
    }
}

#[cfg(test)]
mod tests {
    use mlua::Value;

    use super::*;
    use crate::encode;

    fn vm(kinds: &[ResourceKind]) -> Vm {
        let vm = Vm::new().unwrap();
        let reader: ResourceReader = Rc::new(|r: &Resource| Ok(format!("bytes of {}", r.entry()).into_bytes()));
        vm.install_resources(kinds, reader).unwrap();
        vm.seal().unwrap();
        vm
    }

    #[test]
    fn a_handle_reads_its_own_file() {
        let vm = vm(&ResourceKind::ALL);
        let text: String = vm.load(r#"return host.resource("limits.json"):read()"#, "t").unwrap();
        assert_eq!(text, "bytes of assets/host/limits.json");
        let shown: String = vm.load(r#"return tostring(keyboard.resource("pet/idle.png"))"#, "t").unwrap();
        assert_eq!(shown, r#"keyboard.resource("pet/idle.png")"#);
    }

    #[test]
    fn the_keyboard_has_no_host_table_and_names_are_checked() {
        let vm = vm(&[ResourceKind::Keyboard]);
        assert!(vm.load::<Value>(r#"return host.resource("limits.json")"#, "t").is_err());
        assert!(vm.load::<Value>(r#"return keyboard.resource("../secret")"#, "t").is_err());
        let handle: AnyUserData = vm.load(r#"return keyboard.resource("pet/idle.png")"#, "t").unwrap();
        let ResourceRef::Packaged(resource) = resource_of(&handle).unwrap() else { panic!("a packaged resource") };
        assert_eq!(resource.entry(), "assets/keyboard/pet/idle.png");
    }

    #[test]
    fn live_handles_name_one_word_and_can_only_be_drawn() {
        let vm = Vm::new().unwrap();
        vm.install_live_resources().unwrap();
        vm.seal().unwrap();
        let handle: AnyUserData = vm.load(r#"return live.resource("cover")"#, "t").unwrap();
        assert_eq!(resource_of(&handle).unwrap(), ResourceRef::Live(LiveName::parse("cover").unwrap()));
        assert!(vm.load::<Value>(r#"return live.resource("a/b")"#, "t").is_err());
        let read = vm.load::<Value>(r#"return live.resource("cover"):read()"#, "t").unwrap_err();
        assert!(format!("{read:#}").contains("can only be drawn"), "{read:#}");
    }

    #[test]
    fn handles_are_not_plain_data() {
        let vm = vm(&[ResourceKind::Keyboard]);
        let sent: Value = vm.load(r#"return { icon = keyboard.resource("a.png") }"#, "t").unwrap();
        assert!(encode(vm.lua(), sent).is_err());
    }
}
