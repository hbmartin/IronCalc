use super::history::{Diff, DiffType, QueueDiffs};
use crate::{types::WorkbookSettings, UserModel};

impl UserModel<'_> {
    pub(super) fn with_user_display_edit<T>(
        &mut self,
        record: bool,
        edit: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let outermost = self.model.evaluation.display.edit_depth == 0;
        let journaled = outermost && record;
        if journaled {
            self.model.start_display_journal();
        }
        let entries = self.history.undo_stack.len();
        self.model.begin_display_edit();
        let result = edit(self);
        if result.is_ok() {
            self.model.finish_display_edit(!self.pause_evaluation);
        } else {
            self.model.cancel_display_edit();
        }
        if journaled {
            let difference = self.model.take_display_journal();
            if self.history.undo_stack.len() > entries {
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

impl UserModel<'_> {
    /// Retire ownership at explicit remote writes, not at dependent calculation writes.
    pub(super) fn apply_external_display_diff(
        &mut self,
        diff: &Diff,
        undo: bool,
    ) -> Result<(), String> {
        use crate::{display::DisplayConflicts, expressions::types::Area};
        let mut conflicts = DisplayConflicts::default();
        let area = match diff {
            Diff::SetCellValue {
                sheet, row, column, ..
            } => Some(Area {
                sheet: *sheet,
                row: *row,
                column: *column,
                width: 1,
                height: 1,
            }),
            Diff::SetArrayValue {
                sheet,
                row,
                column,
                width,
                height,
                ..
            }
            | Diff::RangeClearContents {
                sheet,
                row,
                column,
                width,
                height,
                ..
            }
            | Diff::RangeClearAll {
                sheet,
                row,
                column,
                width,
                height,
                ..
            } => Some(Area {
                sheet: *sheet,
                row: *row,
                column: *column,
                width: *width,
                height: *height,
            }),
            _ => None,
        };
        if let Some(ref area) = area {
            conflicts.area(&self.model, area);
        }
        let structural = matches!(
            diff,
            Diff::InsertRows { .. }
                | Diff::DeleteRows { .. }
                | Diff::InsertColumns { .. }
                | Diff::DeleteColumns { .. }
                | Diff::MoveRows { .. }
                | Diff::MoveColumns { .. }
                | Diff::DeleteSheet { .. }
                | Diff::RenameSheet { .. }
                | Diff::NewSheet { .. }
                | Diff::DuplicateSheet { .. }
                | Diff::MoveSheet { .. }
        );
        let structural_sheet = match diff {
            Diff::InsertRows { sheet, .. }
            | Diff::DeleteRows { sheet, .. }
            | Diff::InsertColumns { sheet, .. }
            | Diff::DeleteColumns { sheet, .. }
            | Diff::MoveRows { sheet, .. }
            | Diff::MoveColumns { sheet, .. } => Some(*sheet),
            Diff::RenameSheet { index, .. } => Some(*index),
            Diff::NewSheet { index, .. } if undo => Some(*index),
            Diff::DuplicateSheet { new_index, .. } if undo => Some(*new_index),
            Diff::MoveSheet {
                sheet_index,
                new_index,
            } => Some(if undo { *new_index } else { *sheet_index }),
            _ => None,
        };
        if let Diff::DeleteSheet { old_data, .. } = diff {
            conflicts.sheets.insert(old_data.sheet_id);
        }
        if let Some(sheet) = structural_sheet {
            if let Ok(ws) = self.model.workbook.worksheet(sheet) {
                conflicts.sheets.insert(ws.sheet_id);
            }
        }
        let formulas: Vec<_> = if structural {
            self.model
                .workbook
                .worksheets
                .iter()
                .map(|ws| (ws.sheet_id, ws.shared_formulas.clone()))
                .collect()
        } else {
            Vec::new()
        };
        conflicts.all = match diff {
            Diff::SetLocale {
                old_value,
                new_value,
            } => self.model.workbook.settings.locale != *if undo { old_value } else { new_value },
            Diff::SetTimezone {
                old_value,
                new_value,
            } => self.model.workbook.settings.tz != *if undo { old_value } else { new_value },
            Diff::SetIterativeCalculation {
                old_value,
                new_value,
            } => {
                self.model.workbook.settings.iterative_calculation
                    != *if undo { old_value } else { new_value }
            }
            Diff::CreateDefinedName { .. }
            | Diff::DeleteDefinedName { .. }
            | Diff::UpdateDefinedName { .. } => true,
            _ => false,
        };
        let result = if undo {
            self.apply_undo_diff_list(&vec![diff.clone()])
        } else {
            self.apply_diff_list(&vec![diff.clone()])
        };
        if let Some(ref area) = area {
            conflicts.area(&self.model, area);
        }
        for (id, before) in formulas {
            if self
                .model
                .workbook
                .worksheets
                .iter()
                .find(|ws| ws.sheet_id == id)
                .is_none_or(|ws| ws.shared_formulas != before)
            {
                conflicts.sheets.insert(id);
            }
        }
        for entry in self
            .history
            .undo_stack
            .iter_mut()
            .chain(&mut self.history.redo_stack)
        {
            entry.display.discard_conflicts(&conflicts);
        }
        result
    }
}
