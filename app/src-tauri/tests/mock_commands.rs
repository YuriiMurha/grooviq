//! The e2e mock engine (`app/ui/e2e/mockTauri.ts`) answers exactly the commands the shell
//! registers (#121): a command the shell gains, or drops, fails here instead of leaving the
//! Playwright suite testing a contract the app does not have.

use std::collections::BTreeSet;
use std::path::Path;

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The command names in `lib.rs`'s `generate_handler![...]`.
fn registered() -> BTreeSet<String> {
    let lib = read("src/lib.rs");
    let list = lib
        .split_once("generate_handler![")
        .and_then(|(_, rest)| rest.split_once(']'))
        .expect("lib.rs registers its commands with generate_handler!")
        .0;
    list.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| item.rsplit("::").next().unwrap().to_owned())
        .collect()
}

/// The keys of the mock's `commands` table, without the plugins' (`"plugin:..."`).
fn mocked() -> BTreeSet<String> {
    let mock = read("../ui/e2e/mockTauri.ts");
    let mut lines = mock
        .lines()
        .skip_while(|l| !l.trim_start().eq("const commands = {"));
    assert!(lines.next().is_some(), "the mock has a `commands` table");
    lines
        .take_while(|l| *l != "  } satisfies MockCommands;")
        .filter_map(|l| {
            let key = l.strip_prefix("    ")?.split_once(':')?.0;
            key.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                .then(|| key.to_owned())
        })
        .collect()
}

#[test]
fn the_mock_answers_every_command_the_shell_registers_and_no_other() {
    let registered = registered();
    let mocked = mocked();
    assert!(
        registered.len() > 30,
        "parsed the shell's list: {registered:?}"
    );
    let missing: Vec<_> = registered.difference(&mocked).collect();
    let extra: Vec<_> = mocked.difference(&registered).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "the mock lacks {missing:?}; the shell has no {extra:?}"
    );
}
