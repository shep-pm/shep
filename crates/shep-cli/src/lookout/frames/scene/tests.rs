use ratatui::style::Modifier;

use super::super::gallery::GALLERY_PREAMBLE;
use super::super::scene;
use super::*;

/// Each caption clause in [`Scene::caption`] is pinned by one assertion
/// here, and the preamble's own scene count matches `Scene::ALL`.
///
/// `write_the_gallery` commits the preamble into
/// `docs/lookout/frames.txt`, so a wrong count is a document that
/// miscounts itself for an operator. The count is spelled out, which is
/// the form a reader sees.
#[test]
fn the_gallery_preamble_counts_the_scenes_it_has() {
    const NUMBERS: [(usize, &str); 34] = [
        (34, "thirty-four"),
        (35, "thirty-five"),
        (36, "thirty-six"),
        (37, "thirty-seven"),
        (38, "thirty-eight"),
        (39, "thirty-nine"),
        (40, "forty"),
        (41, "forty-one"),
        (42, "forty-two"),
        (43, "forty-three"),
        (44, "forty-four"),
        (45, "forty-five"),
        (46, "forty-six"),
        (47, "forty-seven"),
        (48, "forty-eight"),
        (49, "forty-nine"),
        (50, "fifty"),
        (51, "fifty-one"),
        (52, "fifty-two"),
        (53, "fifty-three"),
        (54, "fifty-four"),
        (55, "fifty-five"),
        (56, "fifty-six"),
        (57, "fifty-seven"),
        (58, "fifty-eight"),
        (59, "fifty-nine"),
        (60, "sixty"),
        (61, "sixty-one"),
        (62, "sixty-two"),
        (63, "sixty-three"),
        (64, "sixty-four"),
        (65, "sixty-five"),
        (66, "sixty-six"),
        (67, "sixty-seven"),
    ];
    let spelled = NUMBERS
        .iter()
        .find(|(n, _)| *n == Scene::ALL.len())
        .map(|(_, word)| *word)
        .expect("add the next number to NUMBERS when the gallery outgrows it");
    // `fifty` is a prefix of `fifty-one` through `fifty-eight`, so a bare
    // `contains` passes when the preamble overstates inside its own
    // decade: 50 scenes against "fifty-eight" satisfies `contains("fifty")`.
    // Refusing the hyphen makes the match a whole number.
    assert!(
        !GALLERY_PREAMBLE.contains(&format!("{spelled}-")),
        "the preamble spells a larger number than {} scenes: {spelled}-…",
        Scene::ALL.len()
    );
    assert!(
        GALLERY_PREAMBLE.contains(spelled),
        "the preamble says something other than {spelled}, and \
         `write_the_gallery` commits it for an operator to read"
    );
}

/// Only checks that every scene has a caption, since whether the
/// caption is pinned by an assertion is not machine-checkable.
#[test]
fn every_scene_has_a_caption_and_a_distinct_label() {
    let mut labels = std::collections::BTreeSet::new();
    for which in Scene::ALL {
        assert!(
            labels.insert(which.label()),
            "two scenes share {}",
            which.label()
        );
        let caption = which.caption();
        assert!(caption.len() > 30, "{} has a stub caption", which.label());
        assert!(
            caption.ends_with('.'),
            "{}'s caption is not a sentence",
            which.label()
        );
    }
}

/// The scene that follows `scene` in gallery order, or `None` after the
/// last one. Mirrors `Scene::ALL`'s own order.
///
/// Exhaustive over [`Scene`], with no wildcard arm: a variant added to the
/// enum without an arm here fails to compile. The chain itself is what
/// `scene_all_lists_every_variant_the_compiler_can_see` walks, since a
/// variant could get an arm here that loops back on itself.
fn scene_after(scene: Scene) -> Option<Scene> {
    match scene {
        Scene::HealthyWide => Some(Scene::Errored),
        Scene::Errored => Some(Scene::Grouped),
        Scene::Grouped => Some(Scene::Folds),
        Scene::Folds => Some(Scene::WithDogs),
        Scene::WithDogs => Some(Scene::MemCeiling),
        Scene::MemCeiling => Some(Scene::CfgDrift),
        Scene::CfgDrift => Some(Scene::Empty),
        Scene::Empty => Some(Scene::Narrow),
        Scene::Narrow => Some(Scene::TooNarrow),
        Scene::TooNarrow => Some(Scene::Retrying),
        Scene::Retrying => Some(Scene::Frozen),
        Scene::Frozen => Some(Scene::Refused),
        Scene::Refused => Some(Scene::FilterEditing),
        Scene::FilterEditing => Some(Scene::FilterActive),
        Scene::FilterActive => Some(Scene::FilterNoMatch),
        Scene::FilterNoMatch => Some(Scene::NoDetail),
        Scene::NoDetail => Some(Scene::TableOnly),
        Scene::TableOnly => Some(Scene::FeedGap),
        Scene::FeedGap => Some(Scene::FeedMissing),
        Scene::FeedMissing => Some(Scene::Cramped),
        Scene::Cramped => Some(Scene::HostUnknown),
        Scene::HostUnknown => Some(Scene::Lambs),
        Scene::Lambs => Some(Scene::LambsUnknown),
        Scene::LambsUnknown => Some(Scene::Confirm),
        Scene::Confirm => Some(Scene::Acting),
        Scene::Acting => Some(Scene::ActionRefused),
        Scene::ActionRefused => Some(Scene::ActionAccepted),
        Scene::ActionAccepted => Some(Scene::ActionRefusedOffline),
        Scene::ActionRefusedOffline => Some(Scene::SettingsFresh),
        Scene::SettingsFresh => Some(Scene::SettingsSet),
        Scene::SettingsSet => Some(Scene::SettingsConfirm),
        Scene::SettingsConfirm => Some(Scene::SettingsTyping),
        Scene::SettingsTyping => Some(Scene::SettingsDogs),
        Scene::SettingsDogs => Some(Scene::SettingsNarrow),
        Scene::SettingsNarrow => Some(Scene::SettingsShort),
        Scene::SettingsShort => Some(Scene::Bleats),
        Scene::Bleats => Some(Scene::Secrets),
        Scene::Secrets => Some(Scene::SheepPane),
        Scene::SheepPane => Some(Scene::SheepPaneCpuOnly),
        Scene::SheepPaneCpuOnly => Some(Scene::SheepPaneSparklines),
        Scene::SheepPaneSparklines => Some(Scene::SheepPaneShort),
        Scene::SheepPaneShort => Some(Scene::EditPane),
        Scene::EditPane => Some(Scene::EditPaneEdited),
        Scene::EditPaneEdited => Some(Scene::EditPaneSqueezed),
        Scene::EditPaneSqueezed => Some(Scene::EditPaneNarrow),
        Scene::EditPaneNarrow => Some(Scene::SheepDogsRow),
        Scene::SheepDogsRow => Some(Scene::SheepDogsList),
        Scene::SheepDogsList => Some(Scene::SheepDogsRemove),
        Scene::SheepDogsRemove => Some(Scene::SheepDogTable),
        Scene::SheepDogTable => Some(Scene::SheepDogUnion),
        Scene::SheepDogUnion => Some(Scene::DogEntries),
        Scene::DogEntries => Some(Scene::DogEntriesNarrow),
        Scene::DogEntriesNarrow => Some(Scene::DogEntryTyping),
        Scene::DogEntryTyping => Some(Scene::DogEntriesNew),
        Scene::DogEntriesNew => Some(Scene::CloseDialog),
        Scene::CloseDialog => Some(Scene::CloseDialogFloor),
        Scene::CloseDialogFloor => Some(Scene::CloseDialogNarrow),
        Scene::CloseDialogNarrow => Some(Scene::CloseDialogParked),
        Scene::CloseDialogParked => Some(Scene::Keymap),
        Scene::Keymap => Some(Scene::KeymapFloor),
        Scene::KeymapFloor => Some(Scene::KeymapBorderlessWide),
        Scene::KeymapBorderlessWide => Some(Scene::KeymapNarrow),
        Scene::KeymapNarrow => Some(Scene::KeymapTwoColumn),
        Scene::KeymapTwoColumn => Some(Scene::KeymapFrozen),
        Scene::KeymapFrozen => Some(Scene::KeymapReadOnly),
        Scene::KeymapReadOnly => Some(Scene::KeymapShort),
        Scene::KeymapShort => None,
    }
}

/// Walks [`scene_after`] from the first scene and checks the walk names
/// exactly `Scene::ALL`, in the same order.
///
/// The walk can only see variants `scene_after` was taught about, and
/// that match cannot compile with one missing, so a scene left out of
/// `ALL` shows up here as a length or order mismatch rather than as
/// nothing at all.
#[test]
fn scene_all_lists_every_variant_the_compiler_can_see() {
    let mut walked = vec![Scene::HealthyWide];
    while let Some(next) = scene_after(*walked.last().unwrap()) {
        // Bounded, because `scene_after` is a hand-kept chain and a
        // variant wired back at an earlier one would otherwise spin here
        // forever. Nextest has no per-test timeout, so an unbounded walk
        // fails as a job timeout with no named test.
        assert!(
            walked.len() < Scene::ALL.len(),
            "scene_after cycles: walked {} scenes without reaching the end of {}",
            walked.len(),
            Scene::ALL.len()
        );
        walked.push(next);
    }
    assert_eq!(walked.as_slice(), Scene::ALL);
}

/// `sgr` now draws a foreground, a background and `REVERSED`, so the
/// title and section bands' reverse video and the selected row's
/// painted ground both reach `frames.ansi`. This test still forbids
/// any modifier beyond `REVERSED`: nothing in the dashboard uses one
/// today, and a scene that started would render unstyled without this
/// guard noticing.
#[test]
fn no_scene_uses_a_modifier_the_ansi_renderer_cannot_render() {
    for which in Scene::ALL {
        let buffer = scene(*which).1;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let cell = &buffer[(buffer.area.x + x, buffer.area.y + y)];
                assert!(
                    cell.modifier.is_empty() || cell.modifier == Modifier::REVERSED,
                    "{} has an unrendered modifier at {x},{y}: {:?}",
                    which.label(),
                    cell.modifier
                );
            }
        }
    }
}
