//! Resource handles: `keyboard.resource(name)` and `host.resource(name)` return an opaque `Resource`,
//! with the name checked when the handle is made. Each host installs only the tables for resources it
//! has, so `keyboard.luau` can't even name a host resource.

use std::rc::Rc;

use anyhow::Result;
use glassdeck_package::{Resource, ResourceKind, ResourceName};
use mlua::{AnyUserData, MetaMethod, UserData, UserDataMethods};

use crate::{LuaResultExt, Vm};

/// Reads a resource's bytes for `Resource:read()`.
pub type ResourceReader = Rc<dyn Fn(&Resource) -> Result<Vec<u8>>>;

struct Handle(Resource);

impl UserData for Handle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("read", |lua, handle, ()| {
            let reader = lua.app_data_ref::<ResourceReader>().ok_or_else(|| mlua::Error::runtime("resources can't be read here"))?;
            let bytes = reader(&handle.0).map_err(|e| mlua::Error::runtime(format!("{e:#}")))?;
            lua.create_string(bytes)
        });
        methods.add_meta_method(MetaMethod::ToString, |_, handle, ()| Ok(handle.0.to_string()));
    }
}

/// The resource a handle names, for host APIs that take one, like `ui:image`.
pub fn resource_of(value: &AnyUserData) -> mlua::Result<Resource> {
    match value.borrow::<Handle>() {
        Ok(handle) => Ok(handle.0.clone()),
        Err(_) => Err(mlua::Error::runtime("expected a resource from keyboard.resource(...)")),
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
                    Ok(Handle(Resource { kind, name }))
                })
                .lua_context("creating resource")?;
            let table = lua.create_table().lua_context("creating a resource table")?;
            table.set("resource", resource).lua_context("filling a resource table")?;
            lua.globals().set(kind.folder(), table).lua_context("installing a resource table")?;
        }
        lua.set_app_data(reader);
        Ok(())
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
        assert_eq!(resource_of(&handle).unwrap().entry(), "assets/keyboard/pet/idle.png");
    }

    #[test]
    fn handles_are_not_plain_data() {
        let vm = vm(&[ResourceKind::Keyboard]);
        let sent: Value = vm.load(r#"return { icon = keyboard.resource("a.png") }"#, "t").unwrap();
        assert!(encode(vm.lua(), sent).is_err());
    }
}
