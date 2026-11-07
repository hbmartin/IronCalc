use super::history::{Diff, DiffType, QueueDiffs};
use crate::{types::WorkbookSettings, UserModel};

impl UserModel<'_> {
    pub(super) fn with_user_display_edit<T>(
        &mut self,
        record: bool,
        edit: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let outermost = self.model.evaluation.display.edit_depth == 0;
        let before = (outermost && record).then(|| self.model.display_snapshot());
        let entries = self.history.undo_stack.len();
        self.model.begin_display_edit();
        let result = edit(self);
        if result.is_ok() {
            self.model.finish_display_edit(!self.pause_evaluation);
        } else {
            self.model.cancel_display_edit();
        }
        if self.history.undo_stack.len() > entries {
            if let Some(before) = before {
                let difference = before.difference(self.model.display_snapshot());
                if let Some(entry) = self.history.undo_stack.last_mut() {
                    entry.display = difference;
                }
            }
        }
        result
    }

    fn preflight_context_transition(
        &self,
        settings: &mut WorkbookSettings,
        diffs: &[Diff],
        undo: bool,
    ) -> Result<(), String> {
        let visit = |diff: &Diff, settings: &mut WorkbookSettings| -> Result<(), String> {
            match diff {
                Diff::SetIterativeCalculation {
                    old_value,
                    new_value,
                } => {
                    settings.iterative_calculation =
                        if undo { old_value } else { new_value }.clone()
                }
                Diff::SetTimezone {
                    old_value,
                    new_value,
                }
                | Diff::SetLocale {
                    old_value,
                    new_value,
                } => {
                    let new = if undo { old_value } else { new_value };
                    let current = if matches!(diff, Diff::SetTimezone { .. }) {
                        &settings.tz
                    } else {
                        &settings.locale
                    };
                    if current == new {
                        return Ok(());
                    }
                    if self.pause_evaluation {
                        return Err(
                            "Cannot change calculation context while evaluation is paused".into(),
                        );
                    }
                    if settings.iterative_calculation.enabled
                        && settings.iterative_calculation.validate().is_err()
                    {
                        return Err("Cannot change calculation context with invalid iterative calculation settings".into());
                    }
                    if matches!(diff, Diff::SetTimezone { .. }) {
                        crate::tz::Tz::parse(new)?;
                        settings.tz = new.clone();
                    } else {
                        crate::locale::get_locale(new)?;
                        settings.locale = new.clone();
                    }
                }
                _ => {}
            }
            Ok(())
        };
        if undo {
            for diff in diffs.iter().rev() {
                visit(diff, settings)?;
            }
        } else {
            for diff in diffs {
                visit(diff, settings)?;
            }
        }
        Ok(())
    }

    pub(super) fn preflight_context_diffs(&self, diffs: &[Diff], undo: bool) -> Result<(), String> {
        self.preflight_context_transition(&mut self.model.workbook.settings.clone(), diffs, undo)
    }

    pub(super) fn preflight_context_queue(&self, queue: &[QueueDiffs]) -> Result<(), String> {
        let mut settings = self.model.workbook.settings.clone();
        for item in queue {
            self.preflight_context_transition(
                &mut settings,
                &item.list,
                matches!(item.r#type, DiffType::Undo),
            )?;
        }
        Ok(())
    }
}
