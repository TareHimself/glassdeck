//! What the language server sees: Luau's own checker accepts `bridge.luau` written with the wrapper
//! kinds, and each half's generated types check as the scripts will use them.

use std::path::Path;

use glassdeck_luau_check::{LuauChecker, LuauFile};
use glassdeck_messages::{Half, definitions, parse, wrapper_definitions};

const MESSAGES: &str = "type Track = { title: string, artist: string }\nexport type nowPlaying = KeyboardState<Track>\nexport type palette = KeyboardState<{ colours: { Color }, speed: number }>\nexport type tapped = HostEvent<{ key: number }>\n";

fn checker() -> Option<LuauChecker> {
    let native = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/native");
    match LuauChecker::load(&native) {
        Ok(checker) => Some(checker),
        Err(e) => {
            eprintln!("skipped: the Luau checker isn't built in {} ({e:#})", native.display());
            None
        }
    }
}

#[test]
fn messages_luau_is_ordinary_luau_with_the_wrapper_kinds() {
    let Some(checker) = checker() else { return };
    let wrappers = LuauFile { name: "bridge-api.d.luau", text: wrapper_definitions() };
    let problems = checker.check(&[wrappers], LuauFile { name: "bridge.luau", text: MESSAGES }).unwrap();
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn each_halfs_generated_types_check_as_scripts_use_them() {
    let Some(checker) = checker() else { return };
    let messages = parse(MESSAGES).unwrap();
    let keyboard = definitions(&messages, Half::Keyboard, false);
    let script = "--!strict\nlocal heard: MessagesHeard = (nil :: any)\nheard.nowPlaying(function(track) local t: string = track.title end)\nlocal sent: MessagesSent = (nil :: any)\nsent.tapped({ key = 3 })\n";
    let problems = checker.check(&[LuauFile { name: "messages.d.luau", text: &keyboard }], LuauFile { name: "keyboard.luau", text: script }).unwrap();
    assert!(problems.is_empty(), "{problems:#?}");
    let wrong = "--!strict\nlocal sent: MessagesSent = (nil :: any)\nsent.tapped({ key = \"three\" })\n";
    let problems = checker.check(&[LuauFile { name: "messages.d.luau", text: &keyboard }], LuauFile { name: "keyboard.luau", text: wrong }).unwrap();
    assert_eq!(problems.len(), 1, "a string where the key's number goes: {problems:#?}");
}

#[test]
fn host_luau_is_checked_against_its_messages_through_the_real_host_api() {
    use glassdeck_luau_check::definitions::{COMMON, HOST};
    let Some(checker) = checker() else { return };
    let messages = parse(MESSAGES).unwrap();
    let host_messages = definitions(&messages, Half::Host, false);
    let files = [
        LuauFile { name: "host-bridge.d.luau", text: &host_messages },
        LuauFile { name: "options.d.luau", text: "export type Options = {}\nexport type Changed = {}\n" },
        LuauFile { name: "common.d.luau", text: COMMON },
        LuauFile { name: "host.d.luau", text: HOST },
    ];
    let good = "--!strict\nbridge.on.tapped(function(t) local k: number = t.key end)\nkeyboard.bridge.palette({ colours = {}, speed = 2 })\n";
    assert!(checker.check(&files, LuauFile { name: "host.luau", text: good }).unwrap().is_empty());
    let wrong = "--!strict\nkeyboard.bridge.palette({ colours = {}, speed = \"fast\" })\n";
    assert_eq!(checker.check(&files, LuauFile { name: "host.luau", text: wrong }).unwrap().len(), 1, "a string where speed's number goes");
}
