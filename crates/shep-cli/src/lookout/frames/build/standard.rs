//! The two dogs-tab scenes for a dog that follows the settings standard.

use std::path::PathBuf;

use serde_json::{Value, json};
use shep_core::dogs::manifest::Serves;
use shep_core::protocol::Response;

use crate::lookout::app::{App, Effect, KeyPress, Msg, Sent};
use crate::lookout::frames::fixtures::edit_pane_config_view;
use crate::lookout::frames::scene::Scene;
use crate::lookout::pane::{ListedDog, SheepDogEntry, StandardSection};
use crate::lookout::view::fixtures::select_field;

/// `jobs`' one schema as a sheep sees it: its dog-wide `poll` is already
/// left out, which is the point of the frames.
fn sheep_form() -> Value {
    json!({
        "type": "object",
        "properties": {
            "model": { "type": "string" },
            "limit": { "type": "integer" },
            "merge": { "enum": ["ask", "auto"] },
        },
    })
}

/// `jobs` following the standard, with `[jobs]` holding all three keys.
fn standard_jobs(serves: Serves) -> ListedDog {
    let shared = json!({ "model": "sonnet", "limit": 3, "merge": "ask" });
    ListedDog {
        entry: SheepDogEntry {
            name: "jobs".to_string(),
            adopted_path: Some(PathBuf::from("/opt/jobs")),
            schema: Some(sheep_form()),
            standard: Some(StandardSection {
                serves,
                shared: shared.as_object().cloned().expect("an object"),
            }),
        },
        asked: true,
    }
}

/// `api`'s dogs tab with `jobs` opened: `SheepDogInherited` on an opted-in
/// dog and a sheep whose own table sets `limit`, `SheepDogEverySheep` on a
/// dog serving every sheep and a sheep with no table at all.
pub(super) fn open_standard_dog(app: &mut App, which: Scene) {
    let (serves, own) = match which {
        Scene::SheepDogInherited => (Serves::OptedIn, Some(json!({ "limit": 9 }))),
        _ => (Serves::EverySheep, None),
    };
    let mut view = edit_pane_config_view();
    if let Some(own) = own {
        view.config.dogs.insert(
            "jobs".to_string(),
            own.as_object().cloned().expect("an object").into(),
        );
    }
    app.update(Msg::Key(KeyPress::Edit));
    app.update(Msg::Replied {
        sent: Sent::SheepConfig {
            name: "api".to_string(),
        },
        result: Ok(Response::SheepConfig(Box::new(view))),
    });
    let Effect::ListSheepDogs { ask, .. } = app.update(Msg::Key(KeyPress::Group(9))) else {
        panic!("the dogs tab lists its dogs");
    };
    app.update(Msg::SheepDogNames {
        sheep: "api".to_string(),
        ask,
        dogs: vec![standard_jobs(serves)],
    });
    app.update(Msg::Key(KeyPress::Confirm));
    // Onto an inherited row, where the foot says where it comes from.
    select_field(app, "model");
}
