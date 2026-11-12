//! Keeping Sunshine's app list in step with the games found on this PC.
//!
//! Rules:
//! - PocketDeck only edits or removes apps it created itself (remembered in `managed`).
//! - An app the person made in Sunshine with the same name as a game is left alone; the game is
//!   reported as already there.
//! - An app's name and cover never change once made unless the game itself was renamed, so the
//!   Sunshine app ID (CRC of name + cover hash) stays put, and so does the phone's library entry.

use crate::library::LibraryState;
use pocketdeck_library::{Game, Launch};
use pocketdeck_sunshine::api::{Api, ApiError, App};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// What Sunshine should hold for one game.
#[derive(Debug, Clone, PartialEq)]
pub struct Wanted {
    pub key: String,
    pub app: Value,
}

impl Wanted {
    pub fn name(&self) -> &str {
        self.app["name"].as_str().unwrap_or("")
    }
}

pub fn app_json(game: &Game, cover: Option<&Path>) -> Value {
    let (cmd, working_dir, detached) = match &game.launch {
        Launch::Detached { uri } => (String::new(), String::new(), vec![uri.clone()]),
        Launch::Command { cmd, working_dir } => (cmd.clone(), working_dir.clone().unwrap_or_default(), vec![]),
    };
    json!({
        "name": game.name,
        "output": "",
        "cmd": cmd,
        "working-dir": working_dir,
        "image-path": cover.map(|p| p.display().to_string()).unwrap_or_default(),
        "detached": detached,
        "prep-cmd": [],
        "exclude-global-prep-cmd": false,
        "elevated": false,
        "auto-detach": true,
        "wait-all": true,
        "exit-timeout": 5,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Add(Wanted),
    /// Replace the app currently named `current` (ours) with `wanted`.
    Update { current: String, wanted: Wanted },
    /// Remove our app named `name`, made for game `key`.
    Remove { key: String, name: String },
}

/// The fields that matter for deciding whether an app needs rewriting.
fn same(app: &Value, wanted: &Value) -> bool {
    let detached = |v: &Value| v.get("detached").cloned().unwrap_or(json!([]));
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    ["name", "cmd", "working-dir", "image-path"].iter().all(|k| text(app, k) == text(wanted, k))
        && detached(app) == detached(wanted)
}

/// Works out the changes. Returns the operations and the keys of games skipped because the
/// person already has a Sunshine app with that name.
pub fn plan(existing: &[App], wanted: &[Wanted], managed: &BTreeMap<String, String>) -> (Vec<Op>, Vec<String>) {
    let by_name = |name: &str| existing.iter().find(|a| a.name() == name);
    let mut ops = Vec::new();
    let mut skipped = Vec::new();

    for (key, name) in managed {
        if !wanted.iter().any(|w| &w.key == key) && by_name(name).is_some() {
            ops.push(Op::Remove { key: key.clone(), name: name.clone() });
        }
    }

    for w in wanted {
        match managed.get(&w.key) {
            Some(ours) => match by_name(ours) {
                Some(app) if same(&app.json, &w.app) => {}
                Some(_) => ops.push(Op::Update { current: ours.clone(), wanted: w.clone() }),
                // Ours, but removed in Sunshine's web UI: put it back.
                None if by_name(w.name()).is_none() => ops.push(Op::Add(w.clone())),
                None => skipped.push(w.key.clone()),
            },
            None => {
                if by_name(w.name()).is_some() {
                    skipped.push(w.key.clone());
                } else {
                    ops.push(Op::Add(w.clone()));
                }
            }
        }
    }
    (ops, skipped)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    /// Games left out because Sunshine already has an app of that name made by the person.
    pub skipped: Vec<String>,
}

/// Applies the plan through the API, recording what PocketDeck owns in `state.managed`.
/// Sunshine re-sorts after each change, so the list is fetched again before each step.
pub fn apply(api: &Api, wanted: &[Wanted], state: &mut LibraryState) -> Result<Outcome, ApiError> {
    let existing = api.apps()?;
    let (ops, skipped) = plan(&existing, wanted, &state.managed);
    let mut out = Outcome { skipped, ..Default::default() };
    for op in ops {
        let apps = api.apps()?;
        let find = |name: &str| apps.iter().find(|a| a.name() == name).map(|a| a.index);
        match op {
            Op::Add(w) => {
                api.save_app(None, &w.app)?;
                state.managed.insert(w.key.clone(), w.name().to_string());
                out.added += 1;
            }
            Op::Update { current, wanted } => {
                if let Some(i) = find(&current) {
                    api.save_app(Some(i), &wanted.app)?;
                    state.managed.insert(wanted.key.clone(), wanted.name().to_string());
                    out.updated += 1;
                }
            }
            Op::Remove { key, name } => {
                if let Some(i) = find(&name) {
                    api.delete_app(i)?;
                    out.removed += 1;
                }
                state.managed.remove(&key);
            }
        }
    }
    // Forget games whose app is gone and that aren't wanted any more.
    state.managed.retain(|key, _| wanted.iter().any(|w| &w.key == key));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocketdeck_library::Source;

    fn game(key: &str, name: &str) -> Game {
        Game {
            key: key.into(),
            source: Source::Steam,
            name: name.into(),
            launch: Launch::Detached { uri: format!("steam://rungameid/{key}") },
            art: None,
            art_url: None,
        }
    }

    fn wanted(key: &str, name: &str, cover: Option<&str>) -> Wanted {
        Wanted { key: key.into(), app: app_json(&game(key, name), cover.map(Path::new)) }
    }

    fn apps(list: &[Value]) -> Vec<App> {
        list.iter().cloned().enumerate().map(|(index, json)| App { index, json }).collect()
    }

    #[test]
    fn adds_new_games_and_leaves_the_persons_own_apps() {
        let existing = apps(&[json!({"name": "Desktop", "image-path": "desktop.png"}), json!({"name": "Stardew Valley", "cmd": "mine.exe"})]);
        let w = vec![wanted("steam:1", "Stardew Valley", None), wanted("steam:2", "Celeste", None)];
        let (ops, skipped) = plan(&existing, &w, &BTreeMap::new());
        assert_eq!(ops, vec![Op::Add(w[1].clone())]);
        assert_eq!(skipped, vec!["steam:1".to_string()]);
    }

    #[test]
    fn leaves_unchanged_apps_alone() {
        let w = vec![wanted("steam:2", "Celeste", Some(r"C:\c\steam-2.png"))];
        let existing = apps(&[w[0].app.clone()]);
        let managed = BTreeMap::from([("steam:2".to_string(), "Celeste".to_string())]);
        assert_eq!(plan(&existing, &w, &managed), (vec![], vec![]));
    }

    #[test]
    fn updates_and_removes_only_our_apps() {
        let managed = BTreeMap::from([
            ("steam:2".to_string(), "Celeste".to_string()),
            ("steam:3".to_string(), "Hades".to_string()),
        ]);
        let existing = apps(&[
            json!({"name": "Celeste", "detached": ["old"]}),
            json!({"name": "Hades", "detached": ["steam://rungameid/steam:3"]}),
            json!({"name": "Desktop"}),
        ]);
        let w = vec![wanted("steam:2", "Celeste", None)];
        let (ops, _) = plan(&existing, &w, &managed);
        assert_eq!(
            ops,
            vec![
                Op::Remove { key: "steam:3".into(), name: "Hades".into() },
                Op::Update { current: "Celeste".into(), wanted: w[0].clone() },
            ]
        );
    }

    #[test]
    fn restores_our_app_if_deleted_in_sunshine() {
        let managed = BTreeMap::from([("steam:2".to_string(), "Celeste".to_string())]);
        let w = vec![wanted("steam:2", "Celeste", None)];
        assert_eq!(plan(&[], &w, &managed).0, vec![Op::Add(w[0].clone())]);
    }

    #[test]
    fn builds_sunshine_app_entries() {
        let a = app_json(&game("steam:413150", "Stardew Valley"), Some(Path::new(r"C:\x\steam-413150.png")));
        assert_eq!(a["detached"], json!(["steam://rungameid/steam:413150"]));
        assert_eq!(a["cmd"], "");
        assert_eq!(a["image-path"], r"C:\x\steam-413150.png");
        let epic = Game {
            launch: Launch::Command { cmd: "com.epicgames.launcher://apps/a%3Ab%3Ac?action=launch&silent=true".into(), working_dir: None },
            ..game("epic:c", "The Operator")
        };
        let e = app_json(&epic, None);
        assert_eq!(e["detached"], json!([]));
        assert_eq!(e["image-path"], "");
    }
}
