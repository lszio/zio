//! Recipe enforcement (W15).
//!
//! A `Recipe` is a *declaration*: which signal kinds it may consume,
//! which modules it may train, what it may spend. A declaration nobody
//! checks is a comment. This module is where the declaration becomes a
//! gate:
//!
//! * a signal kind the recipe does not consume is refused — a recipe
//!   that silently ignored preference data would make the product's
//!   learning policy an accident;
//! * a run naming an unregistered recipe is refused at the boundary,
//!   not at the first training step, so a typo in a run contract cannot
//!   become a run that trains under an unstated policy;
//! * the budget cap is the recipe's, and the run's ledger is checked
//!   against it.
//!
//! This is additive on purpose: it lives beside the store rather than
//! inside it, and it changes no existing signature.

use crate::contracts::{
    Actor, Error, ErrorKind, LearningSignal, Recipe, Result, Run, SignalKind,
};
use crate::store::Store;

impl Store {
    /// The declared signal kinds, as the human-readable names the API and
    /// the zio side use.
    pub fn recipe_consumes(recipe: &Recipe) -> Vec<&'static str> {
        recipe
            .consumes
            .iter()
            .map(|kind| match kind {
                SignalKind::TeacherLabel => "teacher-label",
                SignalKind::HumanCorrection => "human-correction",
                SignalKind::HumanPreference => "human-preference",
                SignalKind::Demonstration => "demonstration",
                SignalKind::EnvironmentResult => "environment-result",
                SignalKind::Revision => "revision",
                SignalKind::Retraction => "retraction",
            })
            .collect()
    }

    /// Refuse a signal kind the recipe does not declare. `Revision` and
    /// `Retraction` are lifecycle operations on signals rather than
    /// training material, so they are always in force: a recipe cannot
    /// make a retraction unlawful to file.
    pub fn bind_signal_to_recipe(&self, recipe: &Recipe, signal: &LearningSignal) -> Result<()> {
        if matches!(signal.kind, SignalKind::Revision | SignalKind::Retraction) {
            return Ok(());
        }
        if !recipe.consumes.contains(&signal.kind) {
            let allowed = Self::recipe_consumes(recipe).join(", ");
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!(
                    "recipe {} does not consume signal kind {:?} (consumes: {allowed})",
                    recipe.id, signal.kind
                ),
            ));
        }
        // A module the recipe may not train is not a module a
        // demonstration may name as a target.
        if signal.kind == SignalKind::Demonstration {
            if let Some(module) = signal.target_field.as_deref() {
                if !recipe.trainable_modules.is_empty()
                    && !recipe.trainable_modules.iter().any(|m| m == module)
                {
                    return Err(Error::new(
                        ErrorKind::ProtocolViolation,
                        format!(
                            "recipe {} may not train module {module:?} (trainable: {:?})",
                            recipe.id, recipe.trainable_modules
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Load a recipe that must exist. A run bound to an unknown recipe
    /// would train under a policy nobody declared.
    pub fn require_known_recipe(&self, recipe_id: &str) -> Result<Recipe> {
        self.get_recipe(recipe_id).map_err(|e| {
            Error::new(
                ErrorKind::ArtifactUnavailable,
                format!("recipe {recipe_id:?} is not registered: {e}"),
            )
        })
    }

    /// Check a run against its declared recipe: the recipe must exist,
    /// the run's budget may not exceed the recipe's cap, and every
    /// module the run names must be one the recipe may train.
    pub fn check_run_against_recipe(&self, run: &Run) -> Result<Recipe> {
        let recipe = self.require_known_recipe(&run.recipe)?;
        if run.steps_budget > recipe.budget_steps {
            return Err(Error::new(
                ErrorKind::BudgetExhausted,
                format!(
                    "run {} asks for {} steps but recipe {} caps at {}",
                    run.id, run.steps_budget, recipe.id, recipe.budget_steps
                ),
            ));
        }
        if recipe.task_id != run.task_id {
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!(
                    "run {} is on task {:?} but recipe {} declares task {:?}",
                    run.id, run.task_id, recipe.id, recipe.task_id
                ),
            ));
        }
        Ok(recipe)
    }

    /// Every registered recipe, as (id, task, consumes, budget). The
    /// product's module view renders this; it is also the cheapest way
    /// for a caller to see what the store will actually accept.
    pub fn registered_recipes(&self) -> Result<Vec<Recipe>> {
        let mut stmt = self
            .conn
            .prepare("SELECT body, schema FROM recipes ORDER BY id")
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("list recipes: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("list recipes: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            let (body, _schema) = row
                .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("list recipes: {e}")))?;
            out.push(serde_json::from_str(&body).map_err(|e| {
                Error::new(
                    ErrorKind::IncompatibleState,
                    format!("recipe record is unreadable: {e}"),
                )
            })?);
        }
        Ok(out)
    }
}

/// The actor a recipe's signals must come from, by kind. Mirrors the
/// store's own role check so a recipe can be inspected without writing.
pub fn required_role(kind: SignalKind) -> Actor {
    let role = match kind {
        SignalKind::HumanCorrection | SignalKind::HumanPreference | SignalKind::Demonstration => {
            crate::contracts::ActorRole::Annotator
        }
        SignalKind::TeacherLabel => crate::contracts::ActorRole::Operator,
        _ => crate::contracts::ActorRole::Reader,
    };
    Actor::new("recipe-policy", role)
}
