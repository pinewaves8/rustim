//! The declarative lecture plan: a script of steps the engine can play, rewind
//! and report on.
//!
//! A lecture an AI composes is not a bag of commands — it is an ordered set of
//! *steps*, each saying something, arranging something and moving something.
//! Sending them as a stream of `add`/`play` commands works, and it is what the
//! wire has always supported; what it cannot express is the lecture as a
//! **document**: something a host can validate before the first sentence,
//! return to, replay, and ask "where are we?" about. That is the four
//! capabilities this module exists for — preview, rewind, replay, navigation —
//! and they are the difference between a command log and a plan.
//!
//! The wire spelling is four actions:
//!
//! ```text
//! {"action":"plan.load","plan":{"label":"勾股定理","steps":[…]}}
//! {"action":"plan.next"}
//! {"action":"plan.goto","step":2}
//! {"action":"plan.status"}
//! ```
//!
//! # A step
//!
//! A [`PlanStep`] has three parts, all optional but at least one present:
//!
//! * `say` — the line the lecture says when the step begins. It reaches the
//!   narration track (and therefore the live region, the caption band and the
//!   `.srt`), due immediately; what *voice* says it is the host's business, not
//!   the engine's.
//! * `ops` — primitive commands applied before the step's beats, in order. The
//!   same vocabulary a `batch` accepts, including layout and the `explain.*`
//!   templates. They see each other's effects, as batch members do, so a label
//!   created by one op resolves in the next.
//! * `beats` — animation specs queued as successive beats. One spec is one
//!   beat; parallel motion inside a beat is one `group`, which is how the
//!   timeline has always spelled it. The queue appends, so a step never waits
//!   for the previous one to finish — the lecturer decides the pacing, and
//!   [`PlanStatus::playing`] is the honest answer to "is the picture still
//!   moving".
//!
//! # What `plan.load` guarantees
//!
//! Loading is a **pre-flight**, not a stored blob:
//!
//! * The document's shape is checked first — at least one step, no empty step,
//!   nothing to say that trims to nothing, and only primitive members
//!   ([`Plan::check_shape`]).
//! * Every step is then *dry-run* against a scratch copy of the session: its
//!   ops are applied in order, and each beat is built by the same
//!   `command::animation_for` the live `play` uses. So a reference to a label an
//!   earlier step never created, a malformed animation, a missing `into` — all
//!   are refused **before the lecture starts**, under `steps[3].ops[1]` or
//!   `steps[3].beats[0]`.
//!
//! What the dry run deliberately does *not* simulate is the **end state of an
//! animation**: a later op that renames a node a `fade_out` would have removed
//! validates, because the removal happens in the animation's `finish`, not in
//! the spec. That refusal arrives at `plan.next` instead, at the same path.
//!
//! # What `plan.goto` is
//!
//! `goto(n)` restores the picture **at the start of step `n`**: the tree as it
//! was when the plan was loaded, plus steps `0..n` replayed with their beats
//! **settled instantly** — each animation begun, driven to `ease(1.0)` and
//! `finish`ed, which is exactly the reduced-motion path. So:
//!
//! * `goto(0)` is the baseline; `goto(total)` is the lecture's end;
//! * `goto` is **silent** — it says no `say`, because it is a state restore and
//!   not a narration — and the next `plan.next` runs step `n`, narrating it;
//! * `goto` restores the clock to the baseline's, so the same plan and the same
//!   baseline produce the same tree **and the same clock** whatever the session
//!   was doing. It is a function of its argument and the plan, which is what
//!   makes "preview the end before you play it" honest;
//! * the pause flag, the tempo and the motion mode are **not** restored: they
//!   are the viewer's policy, not the lecture's state, and navigation has no
//!   business changing them;
//! * the undo stack is untouched — the plan keeps its own baseline, and the two
//!   histories are independent by design. Narrating a replay is deferred for the
//!   same reason it would be wrong: `goto` is not a re-performance.
//!
//! A refused op during a replay puts the pre-`goto` tree back and answers
//! [`crate::CommandError::InPlanStep`]; the plan and its cursor are unchanged.
//!
//! # What a plan is not
//!
//! It is not a transaction, for `plan.next`: a step's ops run in order and a
//! refusal at op 2 leaves ops 0 and 1 applied, exactly as a `batch` does — with
//! the same recovery, `plan.goto(cursor)` rebuilding the boundary. It is not a
//! second scene format: it names commands and animation specs the engine
//! already has. Named cues (`at_ms` marks for a host's event loop) are deferred;
//! a step boundary is already observable through `plan.status`, and the event
//! stream already reports `beat_started`.

use serde::{Deserialize, Serialize};

use crate::command::{AnimationSpec, Command, CommandError};

/// A whole lecture, as one document.
///
/// Field order is the serde order on the wire: `label`, `steps`. `label` is
/// omitted when absent rather than written as `null`, the same rule every
/// optional payload follows — and it is for humans and logs, never parsed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// A human-readable name for the lecture, e.g. its subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The steps, in the order they play. At least one; see
    /// [`Plan::check_shape`].
    pub steps: Vec<PlanStep>,
}

/// One step of a [`Plan`].
///
/// Field order is the serde order: `say`, `ops`, `beats`. Every part is
/// optional and absent parts are omitted, but a step with none of them is
/// refused — it does nothing, and a model that generated one has a bug worth
/// hearing about before the lecture runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    /// What the lecture says when this step begins, queued as narration due
    /// immediately. Empty or whitespace-only is refused under `say`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub say: Option<String>,
    /// Primitive commands applied before the beats, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ops: Vec<Command>,
    /// Animation beats, queued in order after the ops.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub beats: Vec<AnimationSpec>,
}

/// Where the lecture is, as `plan.status` answers it.
///
/// Field order is the serde order: `loaded`, `label`, `steps`, `cursor`, `say`,
/// `playing`. `label` and `say` are omitted when absent, never `null` — and
/// `say` is the **next** step's line, which is what lets a host speak ahead
/// without a second query.
///
/// `loaded` is explicit rather than inferred from `steps == 0` because a plan
/// can never have zero steps: the field says "there is a plan" in the same
/// place a reader looks for everything else, and the answer to `plan.status`
/// with no plan is a report, not a refusal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStatus {
    /// Whether a plan is loaded at all.
    pub loaded: bool,
    /// The plan's own label, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// How many steps the plan has.
    pub steps: usize,
    /// The index of the next step `plan.next` will run, in `0..=steps`.
    /// `cursor == steps` means the lecture is finished.
    pub cursor: usize,
    /// The next step's `say`, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub say: Option<String>,
    /// Whether the timeline — queued beats or a running animation — is busy.
    /// This is the pacing signal: a step's beats may still be moving when the
    /// host is ready to say the next line.
    pub playing: bool,
}

impl Plan {
    /// The rules a plan's document obeys, checked before anything runs.
    ///
    /// Four structural rules, all refused under the path the client wrote:
    ///
    /// * **At least one step** (`plan.steps`). A plan that plays nothing is a
    ///   lecture with nothing in it; the caller meant `stop` or nothing at all.
    /// * **No empty step** (`steps[i]`). A step with no `say`, no `ops` and no
    ///   `beats` consumes a cursor position and shows nothing — always a
    ///   generation bug, and the one class of mistake a load can catch for free.
    /// * **Something to say** (`steps[i].say`). Whitespace trims to nothing,
    ///   which is exactly the payload a model sends by accident; this is the
    ///   same rule `narrate` applies.
    /// * **Primitive ops** (`steps[i].ops[j]`). The same membership table
    ///   `batch` uses: a step's ops are applied like a list, so a `query`,
    ///   `check`, `plan.status` or nested `batch` inside one has no diff to
    ///   contribute and is refused before anything runs.
    ///
    /// This is the shape half only. References and animation specs are checked
    /// by the scratch dry run [`crate::SceneContext::plan_load`] performs after
    /// it, against the tree the plan will actually start from.
    pub fn check_shape(&self) -> Result<(), CommandError> {
        if self.steps.is_empty() {
            return Err(CommandError::OutOfRange {
                property: "plan.steps".to_owned(),
                detail: "a plan needs at least one step".to_owned(),
            });
        }
        for (index, step) in self.steps.iter().enumerate() {
            if step.say.is_none() && step.ops.is_empty() && step.beats.is_empty() {
                return Err(CommandError::OutOfRange {
                    property: format!("steps[{index}]"),
                    detail: "a step needs a `say`, an `ops` entry or a `beats` entry; \
                             this one does nothing"
                        .to_owned(),
                });
            }
            if let Some(say) = &step.say
                && say.trim().is_empty()
            {
                return Err(CommandError::OutOfRange {
                    property: format!("steps[{index}].say"),
                    detail: "there is nothing to say".to_owned(),
                });
            }
            for (member, op) in step.ops.iter().enumerate() {
                if let Some(why) = crate::command::member_refusal(op) {
                    return Err(CommandError::OutOfRange {
                        property: format!("steps[{index}].ops[{member}]"),
                        detail: why.to_owned(),
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{AnimationKind, AnimationSpec, Property, Target};
    use rustim_mobject::NodeId;

    fn step(say: &str) -> PlanStep {
        PlanStep {
            say: Some(say.to_owned()),
            ..Default::default()
        }
    }

    fn fade(id: NodeId) -> AnimationSpec {
        AnimationSpec {
            target: Some(Target::Id(id)),
            kind: AnimationKind::FadeIn,
            duration: None,
            to: None,
            angle: None,
            factor: None,
            property: None,
            about_point: None,
            into: None,
            path_arc: None,
            animations: None,
            lag_ratio: None,
            rate: None,
        }
    }

    /// The refusal's field path, which is the part a client acts on.
    fn refusal(plan: &Plan) -> (String, String) {
        match plan.check_shape().expect_err("a malformed plan") {
            CommandError::OutOfRange { property, detail } => (property, detail),
            other => panic!("a shape error is out of range: {other:?}"),
        }
    }

    /// A plan with no steps is a lecture with nothing in it, and the path names
    /// the document field rather than an index it does not have.
    #[test]
    fn an_empty_plan_is_refused() {
        let (property, detail) = refusal(&Plan::default());
        assert_eq!(property, "plan.steps");
        assert!(detail.contains("at least one step"), "{detail}");
    }

    /// A step that does nothing consumes a cursor position and shows nothing —
    /// the one generation bug a load catches for free — and the path points at
    /// the step, not at its missing parts.
    #[test]
    fn a_step_that_does_nothing_is_refused_at_its_index() {
        let plan = Plan {
            label: None,
            steps: vec![step("有话说"), PlanStep::default()],
        };
        let (property, detail) = refusal(&plan);
        assert_eq!(property, "steps[1]");
        assert!(detail.contains("does nothing"), "{detail}");
    }

    /// Whitespace trims to nothing, which is the payload a model most easily
    /// sends by accident; the refusal is the same rule `narrate` applies.
    #[test]
    fn a_say_that_trims_to_nothing_is_refused() {
        let plan = Plan {
            label: None,
            steps: vec![step(" \n\t ")],
        };
        assert_eq!(refusal(&plan).0, "steps[0].say");
    }

    /// A list member that answers with something other than a diff has no place
    /// in `ops`; the path counts within the step's own list.
    #[test]
    fn a_read_verb_inside_ops_is_refused_under_its_index() {
        let plan = Plan {
            label: None,
            steps: vec![PlanStep {
                say: None,
                ops: vec![
                    Command::Set {
                        target: Target::Id(NodeId::from_raw(1)),
                        property: Property::Opacity(0.5),
                    },
                    Command::PlanStatus,
                ],
                beats: vec![],
            }],
        };
        assert_eq!(refusal(&plan).0, "steps[0].ops[1]");
    }

    /// The shape half is only structure: a step naming a node that is not there
    /// passes here, because references are the scratch dry run's question and it
    /// needs the tree the plan will start from.
    #[test]
    fn a_well_formed_document_passes_the_shape_check() {
        let plan = Plan {
            label: Some("勾股定理".to_owned()),
            steps: vec![PlanStep {
                say: Some("先看这个三角形".to_owned()),
                ops: vec![Command::Set {
                    target: Target::Id(NodeId::from_raw(1)),
                    property: Property::Opacity(0.5),
                }],
                beats: vec![fade(NodeId::from_raw(1))],
            }],
        };
        plan.check_shape().expect("nothing structural is wrong");
    }

    /// Absent parts are omitted from the document rather than written as
    /// `null`, and the round trip is byte-for-byte: these payloads are model
    /// output, and echoing `"ops":null` back would teach it a protocol that
    /// does not exist.
    #[test]
    fn optional_parts_are_omitted_never_null() {
        let plan = Plan {
            label: Some("标题".to_owned()),
            steps: vec![step("一句话")],
        };
        let json = serde_json::to_string(&plan).expect("json");
        assert_eq!(json, r#"{"label":"标题","steps":[{"say":"一句话"}]}"#);
        assert_eq!(serde_json::from_str::<Plan>(&json).expect("json"), plan);
    }

    /// A status with no plan says so in the same field a loaded one uses, and
    /// absent `label`/`say` vanish rather than becoming nulls.
    #[test]
    fn a_status_omits_what_the_plan_does_not_have() {
        let status = PlanStatus {
            loaded: false,
            label: None,
            steps: 0,
            cursor: 0,
            say: None,
            playing: false,
        };
        let json = serde_json::to_string(&status).expect("json");
        assert_eq!(
            json,
            r#"{"loaded":false,"steps":0,"cursor":0,"playing":false}"#
        );
    }
}
